//! `screenshot`: boot a ROM, run it for a window of frames, and write
//! the visible PPU output to a PNG.
//!
//! The SNES framebuffer is allocated at the worst-case 512x240 (hires +
//! overscan) and never reallocated; the *active* region is reported by
//! `get_display_info()` (256x224 by default). The screenshot therefore
//! crops the active rectangle out of the wider buffer using the full
//! `fb.width` row stride, see [`crop_framebuffer_rgb`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use retro_core::{EmulatorCore, InputState};
use snes1_core::SnesEmulator;

#[derive(Args, Debug)]
pub struct ScreenshotArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Number of frames to run before capturing (~5 s at 60 fps).
    #[arg(long, default_value_t = 300)]
    pub frames: u64,
    /// Output PNG path.
    #[arg(long, short, default_value = "screenshot.png")]
    pub output: PathBuf,
    /// Show a progress bar (on stderr).
    #[arg(long)]
    pub progress: bool,
    /// Isolate a single layer: render only this one (`0`=BG1, `1`=BG2,
    /// `2`=BG3, `3`=BG4, `4`=OBJ). Applies the debugger layer-disable mask
    /// to every other layer, then renders one more frame so the capture
    /// reflects the mask. Diagnoses which layer owns which pixels.
    #[arg(long)]
    pub show_only: Option<u32>,
    /// Hide these layers (comma-separated ids, same numbering as
    /// `--show-only`), keeping the rest. E.g. `--hide 0` drops BG1.
    #[arg(long)]
    pub hide: Option<String>,
    /// Mash these player-1 buttons (comma-separated:
    /// `start,select,a,b,x,y,l,r,up,down,left,right`) during the run.
    /// The buttons are pulsed on/off every few frames so title and menu
    /// screens that gate content (e.g. Mode 5 menus) behind a button
    /// press still advance. Diagnoses input-gated screens the plain
    /// run-N-frames capture can't reach.
    #[arg(long)]
    pub press: Option<String>,
    /// Frame-scheduled player-1 input: `"F:buttons;F:buttons"` (e.g.
    /// `"120:start;132:;180:select;192:"`). Each set-point installs that
    /// mask from frame `F` until the next; an empty list releases. Use to
    /// reach a specific test screen deterministically. Takes precedence
    /// over `--press`.
    #[arg(long)]
    pub press_at: Option<String>,
    /// Inject a console `reset()` at these frame numbers (comma-separated,
    /// e.g. `"360,720"`). Battery SRAM survives a reset, so this drives
    /// reset-gated first-boot routines the plain run can't, most notably
    /// the SPC7110 A/B self-test (press A -> reset -> press B -> reset -> the
    /// game boots), letting a single capture reach real gameplay.
    #[arg(long)]
    pub reset_at: Option<String>,
    /// Show the full overscan frame (240/480 tall) instead of cropping
    /// to the 224-line NTSC TV window. Off by default, matching
    /// bsnes-plus / snes9x output.
    #[arg(long)]
    pub overscan: bool,
    /// Load low-confidence ROMs (blank title / bad checksum) by forcing
    /// the vector-implied mapping instead of failing the detector gate.
    #[arg(long)]
    pub force_detect: bool,
    /// After the run, print each installed coprocessor's live state (the
    /// same dump as `info`). Ties "take a screenshot" and "confirm the
    /// coprocessor did its job", e.g. S-DD1 `DECOMPS > 0` + DMA shadow,
    /// into one command.
    #[arg(long)]
    pub coprocessor_state: bool,
    /// DSP-1 firmware + coprocessor-mode flags.
    #[command(flatten)]
    pub coprocessor: super::CoprocessorArgs,
}

pub fn run(args: ScreenshotArgs) -> Result<()> {
    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    args.coprocessor.apply(&mut emu)?;
    emu.set_show_overscan(args.overscan);
    super::apply_force_detect(&mut emu, &rom, args.force_detect);
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

    if let Some(info) = emu.get_rom_info() {
        println!("Loaded {:?} [{}]", info.title.trim(), info.region);
    }
    if let Some(chip) = emu.coprocessor_label() {
        println!("Coprocessor: {chip}");
    }

    let press_mask = super::parse_button_mask(args.press.as_deref())?;
    let schedule = super::parse_press_schedule(args.press_at.as_deref())?;
    let reset_frames = super::parse_reset_frames(args.reset_at.as_deref())?;

    let pb = make_bar(args.progress, args.frames);
    for i in 0..args.frames {
        // A scheduled reset fires before the frame runs, mirroring a user
        // hitting the console reset button at that point. SRAM/RTC persist
        // (the cartridge is untouched), so a first-boot self-test that
        // gates on SRAM advances across it.
        if reset_frames.contains(&i) {
            emu.reset();
        }
        if let Some(sched) = &schedule {
            // Frame-scheduled program: install the mask in effect on this
            // frame (untouched before the first set-point).
            if let Some(buttons) = sched.mask_at(i) {
                emu.set_input(0, InputState { buttons });
            }
        } else if let Some(mask) = press_mask {
            // Pulse the requested buttons (6 frames held, 6 released) so a
            // title/menu that needs a *press edge*, not a continuous hold,
            // advances toward the gated screen.
            let held = (i / 6) % 2 == 0;
            emu.set_input(
                0,
                InputState {
                    buttons: if held { mask } else { 0 },
                },
            );
        }
        emu.run_frame();
        if let Some(pb) = &pb {
            pb.inc(1);
        }
    }
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    // Optional layer isolation: apply the debugger layer-disable mask,
    // then render one more frame so the framebuffer reflects it (the
    // mask is a `Ppu` field the game's per-frame writes don't touch).
    let hide: Vec<u32> = args
        .hide
        .as_deref()
        .map(|s| s.split(',').filter_map(|t| t.trim().parse().ok()).collect())
        .unwrap_or_default();
    if args.show_only.is_some() || !hide.is_empty() {
        {
            let vd = emu.video_debugger();
            for layer in 0..5u32 {
                let visible = match args.show_only {
                    Some(only) => layer == only,
                    None => !hide.contains(&layer),
                };
                vd.toggle_layer(layer, visible);
            }
        }
        emu.run_frame();
    }

    if args.coprocessor_state {
        super::print_coprocessors(&emu);
    }

    // A forced-blank or zero-brightness frame is legal but renders all
    // black, surface it so a black PNG isn't mistaken for a capture bug.
    if emu.ppu().display_disable() {
        eprintln!(
            "warning: PPU is in forced blank (display_disable set) after {} frames - \
             the image will be black",
            args.frames
        );
    } else if emu.ppu().display_brightness() == 0 {
        eprintln!(
            "warning: PPU master brightness is 0 after {} frames - the image will be black",
            args.frames
        );
    }

    let (disp_w, disp_h) = {
        let disp = emu.get_display_info();
        (disp.width, disp.height)
    };
    let fb = emu.get_framebuffer();
    let (w, h, rgb) = crop_framebuffer_rgb(&fb.data, fb.width, fb.height, disp_w, disp_h);

    let img = image::RgbImage::from_raw(w, h, rgb)
        .ok_or_else(|| anyhow::anyhow!("failed to build {w}x{h} image from framebuffer"))?;
    img.save(&args.output)
        .with_context(|| format!("failed to save screenshot: {}", args.output.display()))?;

    println!(
        "Saved {}x{} screenshot to {} (after {} frames)",
        w,
        h,
        args.output.display(),
        args.frames
    );
    Ok(())
}

/// The active-rectangle crop every PNG-producing command uses; it lives
/// with the golden harness so the picture a golden hashes and the picture
/// a screenshot shows are the same bytes.
pub(crate) use snes1_core::golden::crop_framebuffer_rgb;

fn make_bar(show: bool, len: u64) -> Option<ProgressBar> {
    if !show {
        return None;
    }
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} frames ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );
    Some(pb)
}
