//! `screenshot`: run a ROM for N frames and save the framebuffer as a
//! PNG, with optional controller input, per-pixel composition logging,
//! layer-separated dumps, and VRAM patching for graphics debugging.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::vdp::RenderLayerMode;

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
    /// screenshot. `_plane_a` and `_window` each show only their own part of
    /// the screen: the window replaces plane A where it covers. Transparent
    /// pixels are magenta, so black text on a plane stays visible.
    #[arg(long)]
    pub render_layers: bool,
    /// Patch VRAM word(s) before the last frame render (for debugging).
    /// Format: `ADDR=VALUE` (hex, e.g. `C6A6=825C`). Repeatable.
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

    println!("Running {} frames...", args.frames);
    for f in 0..args.frames {
        // Enable per-pixel logging only on the last frame.
        if f == args.frames - 1 {
            if let Some((x, y)) = dbg_px {
                emu.vdp_mut().set_debug_pixel(x, y);
            }
        }
        super::apply_press(&mut emu, schedule.as_ref(), f);
        emu.run_frame();
    }

    // Apply VRAM patches after running, then re-render one frame so the
    // patched words are visible in the saved screenshot.
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
        emu.run_frame();
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

/// Re-render the current scene through each isolated layer mode and save
/// a PNG per layer next to `output_path`.
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

    let layer_modes = [
        (RenderLayerMode::PlaneB, "plane_b"),
        (RenderLayerMode::PlaneA, "plane_a"),
        (RenderLayerMode::Window, "window"),
        (RenderLayerMode::Sprites, "sprites"),
        (RenderLayerMode::PriorityOverlay, "priority"),
        (RenderLayerMode::SpritePriority, "sprite_priority"),
    ];

    for (mode, suffix) in layer_modes {
        emu.vdp_mut().set_render_layer_mode(mode);
        // Advance one frame so the VDP re-renders the scene in this mode.
        emu.run_frame();
        let layer_path = parent.join(format!("{stem}_{suffix}.{ext}"));
        super::save_screenshot(emu, &layer_path)?;
    }

    emu.vdp_mut().set_render_layer_mode(RenderLayerMode::Normal);
    Ok(())
}
