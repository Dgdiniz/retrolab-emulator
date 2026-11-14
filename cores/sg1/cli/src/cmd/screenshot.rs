//! `screenshot`: run a ROM for N frames and save the framebuffer as a
//! PNG, with optional controller input, per-pixel composition logging,
//! layer-separated dumps, and VRAM patching for graphics debugging.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_core::video::PixelImage;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::vdp::{
    CaptureRecording, RenderLayerMode, LAYER_PLANE_A, LAYER_PLANE_B, LAYER_SPRITES, LAYER_WINDOW,
};

#[derive(Args, Debug)]
pub struct ScreenshotArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Number of frames to run before capturing.
    #[arg(short, long, default_value_t = 1)]
    pub frames: u64,
    /// Output PNG file path.
    #[arg(short, long, default_value = "screenshot.png")]
    pub output: PathBuf,
    /// Frame-scheduled player-1 input: `"F:buttons;F:buttons"` (e.g.
    /// `"180:start;186:;240:a+c"`). Each set-point holds its mask from
    /// frame `F` until the next; an empty list releases. Buttons:
    /// up,down,left,right,a,b,c,start (+x,y,z,mode with --six-button).
    #[arg(long)]
    pub press_at: Option<String>,
    /// Connect 6-button pads so `x`/`y`/`z`/`mode` reach the game.
    #[arg(long)]
    pub six_button: bool,
    /// Log per-pixel composition data for a screen coordinate `X,Y`
    /// (e.g. `160,120`). Output goes to stderr.
    #[arg(long)]
    pub debug_pixel: Option<String>,
    /// Also save layer-separated PNGs (`_plane_b`, `_plane_a`, `_window`,
    /// `_sprites`, `_priority`, `_sprite_priority`) alongside the main
    /// screenshot, all of the screenshot's frame: it stops where its
    /// picture is drawn and vertical blanking has not begun, and the line
    /// states are recorded from the first frame on, so each line of a
    /// layer is drawn from the state it was drawn with (its scroll, palette,
    /// registers and the video memory it read): a raster effect, a scroll
    /// or palette changed part of the way down the screen, shows in the
    /// layers as it shows in the screenshot. The four layers keep
    /// transparency: a pixel the layer does not cover has alpha 0.
    /// `_plane_a` and `_window` each show only their own part of the
    /// screen: the window replaces plane A where it covers. The two
    /// priority views are opaque.
    #[arg(long)]
    pub render_layers: bool,
    /// Patch VRAM word(s) before the last frame render (for debugging):
    /// after the frames run, the words are written and one more frame is
    /// drawn for the screenshot. Format: `ADDR=VALUE` (hex, e.g.
    /// `C6A6=825C`). Repeatable.
    #[arg(long)]
    pub patch_vram: Vec<String>,
    /// After capturing, print the installed coprocessor(s) and their live
    /// state (e.g. the SVP's SSP1601 PC + cycle count on Virtua Racing).
    #[arg(long)]
    pub coprocessor_state: bool,
}

pub fn run(args: ScreenshotArgs) -> Result<()> {
    let (_, mut emu) = super::load_rom(&args.rom)?;
    super::print_rom_title(&emu);

    if args.six_button {
        super::connect_controllers(&mut emu, true);
    }

    let dbg_px = parse_debug_pixel(args.debug_pixel.as_deref())?;
    let vram_patches = parse_vram_patches(&args.patch_vram)?;
    let schedule = super::parse_press_schedule(args.press_at.as_deref())?;

    emu.debugger().set_execution_mode(ExecutionMode::Running);

    // The layers are drawn line by line from the state each line was
    // drawn with: record it while the frames run. Recording changes
    // nothing the game does, so the screenshot is the same either way.
    if args.render_layers {
        emu.set_capture_recording(CaptureRecording {
            line_states: true,
            ..CaptureRecording::default()
        });
    }

    println!("Running {} frames...", args.frames);
    for f in 0..args.frames {
        let last = f == args.frames - 1;
        // Enable per-pixel logging only on the last frame.
        if last {
            if let Some((x, y)) = dbg_px {
                emu.vdp_mut().set_debug_pixel(x, y);
            }
        }
        super::apply_press(&mut emu, schedule.as_ref(), f);
        // The frame of the screenshot stops at the start of its vertical
        // blanking, before the game uploads the next frame's graphics, so
        // the layers below are drawn from the memory its picture was.
        if last && vram_patches.is_empty() {
            emu.run_until_vblank();
        } else {
            emu.run_frame();
        }
    }

    // Apply VRAM patches after running, then draw one more frame, stopped
    // the same way, so the patched words are visible in the saved
    // screenshot.
    if !vram_patches.is_empty() {
        for &(addr, value) in &vram_patches {
            let vram = emu.vdp_mut().get_vram_mut();
            let a = addr as usize;
            if a + 1 < vram.len() {
                vram[a] = (value >> 8) as u8;
                vram[a + 1] = (value & 0xFF) as u8;
                eprintln!("[patch-vram] addr=0x{addr:04X} = 0x{value:04X}");
            }
        }
        emu.run_until_vblank();
    }

    emu.vdp_mut().clear_debug_pixel();

    super::save_screenshot(&emu, &args.output)?;

    if args.render_layers {
        save_layer_screenshots(&mut emu, &args.output)?;
    }

    if args.coprocessor_state {
        println!();
        super::print_coprocessors(&emu);
    }

    Ok(())
}

fn parse_debug_pixel(spec: Option<&str>) -> Result<Option<(u16, u16)>> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() != 2 {
        anyhow::bail!("invalid --debug-pixel '{spec}' (expected X,Y, e.g. 160,120)");
    }
    let x: u16 = parts[0]
        .trim()
        .parse()
        .with_context(|| format!("invalid X in --debug-pixel: {}", parts[0]))?;
    let y: u16 = parts[1]
        .trim()
        .parse()
        .with_context(|| format!("invalid Y in --debug-pixel: {}", parts[1]))?;
    Ok(Some((x, y)))
}

fn parse_vram_patches(specs: &[String]) -> Result<Vec<(u16, u16)>> {
    let mut patches = Vec::new();
    for spec in specs {
        let (addr_s, value_s) = spec.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("invalid --patch-vram '{spec}' (expected ADDR=VALUE)")
        })?;
        let addr = u16::from_str_radix(addr_s.trim(), 16)
            .map_err(|_| anyhow::anyhow!("invalid hex address '{addr_s}' in --patch-vram"))?;
        let value = u16::from_str_radix(value_s.trim(), 16)
            .map_err(|_| anyhow::anyhow!("invalid hex value '{value_s}' in --patch-vram"))?;
        patches.push((addr, value));
    }
    Ok(patches)
}

/// Draw each layer of the held frame alone, and the two priority views,
/// and save a PNG of each next to `output_path`. Nothing runs between them,
/// so all six come from the frame of the screenshot, each line from the
/// line table the run recorded.
fn save_layer_screenshots(emu: &mut sg1_core::GenesisEmulator, output_path: &Path) -> Result<()> {
    let stem = output_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "screenshot".to_string());
    let ext = output_path
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "png".to_string());
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    let path_for = |suffix: &str| parent.join(format!("{stem}_{suffix}.{ext}"));

    let layers = [
        (LAYER_PLANE_B, "plane_b"),
        (LAYER_PLANE_A, "plane_a"),
        (LAYER_WINDOW, "window"),
        (LAYER_SPRITES, "sprites"),
    ];
    for (layer, suffix) in layers {
        let layer_image = emu
            .render_layer(layer as u32)
            .ok_or_else(|| anyhow::anyhow!("no layer {layer}"))?;
        if !layer_image.from_line_states {
            eprintln!(
                "[render-layers] {suffix}: some lines were drawn from the state at the frame's end"
            );
        }
        save_rgba(&layer_image.image, &path_for(suffix))?;
    }

    let views = [
        (RenderLayerMode::PriorityOverlay, "priority"),
        (RenderLayerMode::SpritePriority, "sprite_priority"),
    ];
    for (mode, suffix) in views {
        let image = emu.vdp_mut().render_mode_image(mode);
        save_rgba(&image, &path_for(suffix))?;
    }
    Ok(())
}

/// Save an RGBA picture as a PNG with its alpha.
fn save_rgba(image: &PixelImage, path: &Path) -> Result<()> {
    let png = image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "failed to build {}x{} image from the layer",
                image.width,
                image.height
            )
        })?;
    png.save(path)
        .with_context(|| format!("failed to save layer: {}", path.display()))?;
    println!(
        "Layer saved: {} ({}x{})",
        path.display(),
        image.width,
        image.height
    );
    Ok(())
}
