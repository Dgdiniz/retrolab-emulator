//! Unit tests of the VDP: construction and reset, the picture's size, a
//! picture drawn again, and the debugger's readers. The tests of the ports,
//! the renderer, the DMA and the interrupts are in the files beside this one.

use super::*;

mod dma;
mod interrupts;
mod ports;
mod render;

#[test]
fn test_vdp_creation() {
    let vdp = Vdp::new();
    assert_eq!(vdp.vram.len(), 0x10000);
    assert_eq!(vdp.cram.len(), 64);
    assert_eq!(vdp.vsram.len(), 64);
    assert_eq!(vdp.framebuffer.len(), 320 * 224 * 4);
}

#[test]
fn rerender_current_frame_reflects_layer_mask() {
    let mut vdp = Vdp::new();

    // Display on, V28 (224 lines), H32.
    vdp.registers[1] = 0x44; // display enable (bit6) + bit2 (mode5)
    vdp.registers[12] = 0x00; // H32

    // Plane A nametable at 0xE000 (reg2 bits 5-3), Plane B at 0x0000
    // (reg4=0). A non-overlapping base keeps the nametable clear of the
    // tile we author at index 1.
    vdp.registers[2] = 0x38;

    // Author tile #1 (VRAM 32..64) fully as palette colour 1.
    for b in 0..32 {
        vdp.vram[32 + b] = 0x11;
    }
    // Plane A cell (0,0) -> tile #1, palette 0.
    vdp.vram[0xE000] = 0x00;
    vdp.vram[0xE001] = 0x01;

    // Distinct palette entries: colour 1 = bright blue (plane), colour
    // 2 = bright red (backdrop via reg7 -> palette 0 colour 2).
    vdp.cram[1] = 0x0E00;
    vdp.cram[2] = 0x000E;
    vdp.registers[7] = 0x02;
    vdp.rebuild_cram_rgb();

    let plane_rgb = vdp.cram_rgb[1];
    let backdrop_rgb = vdp.cram_rgb[2];
    assert_ne!(plane_rgb, backdrop_rgb, "test colours must differ");

    // Paused re-render with the layer visible paints the plane colour.
    vdp.rerender_current_frame();
    let px = |v: &Vdp| (v.framebuffer[0], v.framebuffer[1], v.framebuffer[2]);
    assert_eq!(px(&vdp), plane_rgb, "plane A should be visible at (0,0)");

    // Hide Plane A and re-render the *same* frame: (0,0) falls back to
    // the backdrop without the emulator advancing a single cycle.
    vdp.set_layer_debug_disabled(LAYER_PLANE_A, true);
    vdp.rerender_current_frame();
    assert_eq!(px(&vdp), backdrop_rgb, "hidden plane A exposes backdrop");

    // Re-enabling restores the plane on the next paused re-render.
    vdp.set_layer_debug_disabled(LAYER_PLANE_A, false);
    vdp.rerender_current_frame();
    assert_eq!(px(&vdp), plane_rgb, "re-enabled plane A returns");
}

/// A VDP in the 32-cell mode with the display on, tile 1 all colour 1
/// in cell (0, 0) of plane A, and a backdrop of another colour.
fn vdp_with_one_tile() -> Vdp {
    let mut vdp = Vdp::new();
    vdp.registers[1] = 0x44; // display on, mode 5
    vdp.registers[12] = 0x00; // 32 cells
    vdp.registers[2] = 0x38; // plane A at 0xE000
    vdp.registers[7] = 0x02; // backdrop: colour 2
    vdp.vram[32..64].fill(0x11);
    vdp.vram[0xE001] = 0x01;
    vdp.cram[1] = 0x0E00;
    vdp.cram[2] = 0x000E;
    vdp.rebuild_cram_rgb();
    vdp
}

#[test]
fn a_layer_is_drawn_alone_alpha_keyed_and_leaves_the_frame_alone() {
    let mut vdp = vdp_with_one_tile();
    // Plane A hidden in the debugger: the layer asked for is drawn
    // anyway, and the toggle stays.
    vdp.set_layer_debug_disabled(LAYER_PLANE_A, true);
    let framebuffer = vdp.framebuffer.clone();

    let plane_a = vdp
        .render_layer_image(LAYER_PLANE_A)
        .expect("plane A")
        .image;
    assert_eq!((plane_a.width, plane_a.height), (256, 224));
    let (r, g, b) = vdp.cram_rgb[1];
    assert_eq!(plane_a.rgba[..4], [r, g, b, 255], "the tile's colour");
    assert_eq!(
        plane_a.rgba[8 * 4..8 * 4 + 4],
        [0, 0, 0, 0],
        "tile 0 next to it is transparent, not the backdrop"
    );
    assert!(vdp.layer_debug_disabled(LAYER_PLANE_A));
    assert_eq!(vdp.framebuffer, framebuffer, "the framebuffer is untouched");

    let plane_b = vdp
        .render_layer_image(LAYER_PLANE_B)
        .expect("plane B")
        .image;
    assert!(plane_b.rgba.iter().all(|&byte| byte == 0));
    assert!(vdp.render_layer_image(LAYER_COUNT).is_none());

    // With the display off no layer covers anything.
    vdp.registers[1] = 0x04;
    let plane_a = vdp
        .render_layer_image(LAYER_PLANE_A)
        .expect("plane A")
        .image;
    assert!(plane_a.rgba.iter().all(|&byte| byte == 0));
}

#[test]
fn a_picture_drawn_again_puts_back_what_drawing_leaves() {
    let mut vdp = vdp_with_one_tile();
    // Line 7 composited, with flags and a parsed sprite list the frame
    // left: what a repaint or a layer must hand back.
    vdp.composite_scanline(7);
    vdp.sprite_overflow = true;
    vdp.sprite_collision = false;
    vdp.sprite_mask_overflow = true;
    vdp.sprite_cache_valid = false;
    let line_index = vdp.line_index.clone();
    let line_plane_a = vdp.line_plane_a.clone();

    vdp.rerender_current_frame();
    vdp.render_layer_image(LAYER_SPRITES);
    let overlay = vdp.render_mode_image(RenderLayerMode::PriorityOverlay);
    assert_eq!((overlay.width, overlay.height), (256, 224));
    assert!(overlay
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[3] == 255));

    assert!(vdp.sprite_overflow && !vdp.sprite_collision && vdp.sprite_mask_overflow);
    assert!(
        !vdp.sprite_cache_valid,
        "the sprite list is parsed again later"
    );
    assert_eq!(vdp.line_index, line_index);
    assert_eq!(vdp.line_plane_a, line_plane_a);
    assert_eq!(vdp.render_layer_mode, RenderLayerMode::Normal);
}

#[test]
fn the_drawn_height_is_the_mode_s_cut_to_the_framebuffer() {
    let mut vdp = Vdp::new();
    assert_eq!(vdp.drawn_height(), 224);
    vdp.registers[1] |= 0x08; // 30-cell lines
    assert_eq!(vdp.display_height(), 240);
    assert_eq!(vdp.drawn_height(), 224);
    assert_eq!(vdp.first_vblank_line(), 224);
    vdp.set_region(timing::VideoRegion::Pal);
    assert_eq!(vdp.first_vblank_line(), 240);
}

#[test]
fn test_vdp_reset() {
    let mut vdp = Vdp::new();

    // Modify some state
    vdp.registers[0] = 0xFF;
    vdp.address = 0x1234;
    vdp.vram[0] = 0xAB;

    // Reset
    vdp.reset();

    // Check defaults
    assert_eq!(vdp.registers[0], 0x04);
    assert_eq!(vdp.registers[1], 0x04);
    assert_eq!(vdp.registers[12], 0x81);
    assert_eq!(vdp.registers[15], 0x02);
    assert_eq!(vdp.address, 0);
    assert_eq!(vdp.vram[0], 0);
}

#[test]
fn test_display_dimensions() {
    let mut vdp = Vdp::new();

    // Default: H40, V28
    assert_eq!(vdp.display_width(), 320);
    assert_eq!(vdp.display_height(), 224);

    // H32 mode
    vdp.registers[12] &= !0x01;
    assert_eq!(vdp.display_width(), 256);

    // V30 mode
    vdp.registers[1] |= 0x08;
    assert_eq!(vdp.display_height(), 240);
}

#[test]
fn the_window_nametable_is_4kb_aligned_in_h40() {
    let mut vdp = Vdp::new();
    vdp.set_test_register(3, 0x3E); // address bits 15-11 = 0xF800
    vdp.set_test_register(12, 0x00); // H32
    assert_eq!(vdp.get_window_nametable(), 0xF800);
    vdp.set_test_register(12, 0x81); // H40 ignores address bit 11
    assert_eq!(vdp.get_window_nametable(), 0xF000);
}
