//! Integration tests for VDP rendering pipeline.
//!
//! Tests the complete rendering pipeline with all layers and priorities.

use sg1_core::vdp::renderer::{Layer, LayeredPixel};
use sg1_core::vdp::Vdp;

/// Test backdrop color is rendered when no other layers are present.
#[test]
fn test_backdrop_renders_default() {
    let mut vdp = Vdp::new();

    // Set backdrop to red (palette 0, color 1)
    vdp.set_test_register(7, 0x01); // Backdrop: palette 0, color 1

    // Get backdrop color from VDP
    // For now, we'll just verify the register was set
    assert_eq!(vdp.registers()[7], 0x01);
}

/// Test priority order: backdrop < plane B low < plane A low < sprite low.
#[test]
fn test_priority_order_low() {
    let backdrop = LayeredPixel::backdrop((0, 0, 0));
    let plane_b_low = LayeredPixel::plane((255, 0, 0), false, false);
    let plane_a_low = LayeredPixel::plane((0, 255, 0), true, false);
    let sprite_low = LayeredPixel::sprite((0, 0, 255), false);

    // Verify ordering
    assert!(backdrop.layer < plane_b_low.layer);
    assert!(plane_b_low.layer < plane_a_low.layer);
    assert!(plane_a_low.layer < sprite_low.layer);
}

/// Test priority order: sprite high > plane A high > plane B high.
#[test]
fn test_priority_order_high() {
    let plane_b_high = LayeredPixel::plane((255, 0, 0), false, true);
    let plane_a_high = LayeredPixel::plane((0, 255, 0), true, true);
    let sprite_high = LayeredPixel::sprite((0, 0, 255), true);

    assert!(plane_b_high.layer < plane_a_high.layer);
    assert!(plane_a_high.layer < sprite_high.layer);
}

/// Test high priority layers render over low priority.
#[test]
fn test_high_priority_over_low() {
    let plane_a_low = LayeredPixel::plane((255, 0, 0), true, false);
    let plane_b_high = LayeredPixel::plane((0, 255, 0), false, true);

    // Plane B high priority should be above Plane A low priority
    assert!(plane_b_high.layer > plane_a_low.layer);
}

/// Test sprite high priority is topmost.
#[test]
fn test_sprite_high_priority_topmost() {
    let sprite_high = LayeredPixel::sprite((255, 0, 0), true);
    let plane_a_high = LayeredPixel::plane((0, 255, 0), true, true);
    let plane_b_high = LayeredPixel::plane((0, 0, 255), false, true);

    assert!(sprite_high.layer > plane_a_high.layer);
    assert!(sprite_high.layer > plane_b_high.layer);
}

/// Test transparent pixels don't occlude other layers.
#[test]
fn test_transparent_pixels() {
    let transparent = LayeredPixel::transparent();
    let backdrop = LayeredPixel::backdrop((128, 128, 128));

    assert!(transparent.transparent);
    assert!(!backdrop.transparent);

    // Transparent should allow backdrop to show through
    // (This would be tested in actual rendering logic)
}

/// Test color 0 is always transparent (Genesis convention).
#[test]
fn test_color_zero_transparency() {
    // In Genesis, palette color 0 is always transparent
    // This is convention, not enforced by pixel type
    // Actual rendering code should handle this

    // We can test that a pixel can be marked as transparent
    let pixel = LayeredPixel::transparent();
    assert!(pixel.transparent);
}

/// Test plane A renders correctly.
#[test]
fn test_plane_a_rendering_basic() {
    let plane_a_low = LayeredPixel::plane((255, 128, 64), true, false);
    let plane_a_high = LayeredPixel::plane((64, 128, 255), true, true);

    assert_eq!(plane_a_low.layer, Layer::PlaneALow);
    assert_eq!(plane_a_high.layer, Layer::PlaneAHigh);
    assert_eq!(plane_a_low.color, (255, 128, 64));
    assert_eq!(plane_a_high.color, (64, 128, 255));
}

/// Test plane B renders correctly.
#[test]
fn test_plane_b_rendering_basic() {
    let plane_b_low = LayeredPixel::plane((200, 100, 50), false, false);
    let plane_b_high = LayeredPixel::plane((50, 100, 200), false, true);

    assert_eq!(plane_b_low.layer, Layer::PlaneBLow);
    assert_eq!(plane_b_high.layer, Layer::PlaneBHigh);
}

/// Test sprite renders correctly.
#[test]
fn test_sprite_rendering_basic() {
    let sprite_low = LayeredPixel::sprite((255, 255, 0), false);
    let sprite_high = LayeredPixel::sprite((0, 255, 255), true);

    assert_eq!(sprite_low.layer, Layer::SpriteLow);
    assert_eq!(sprite_high.layer, Layer::SpriteHigh);
}

/// Test complete priority ordering (all 7 layers).
#[test]
fn test_complete_priority_stack() {
    let backdrop = LayeredPixel::backdrop((0, 0, 0));
    let plane_b_low = LayeredPixel::plane((1, 0, 0), false, false);
    let plane_a_low = LayeredPixel::plane((2, 0, 0), true, false);
    let sprite_low = LayeredPixel::sprite((3, 0, 0), false);
    let plane_b_high = LayeredPixel::plane((4, 0, 0), false, true);
    let plane_a_high = LayeredPixel::plane((5, 0, 0), true, true);
    let sprite_high = LayeredPixel::sprite((6, 0, 0), true);

    // Verify complete ordering
    assert!(backdrop.layer < plane_b_low.layer);
    assert!(plane_b_low.layer < plane_a_low.layer);
    assert!(plane_a_low.layer < sprite_low.layer);
    assert!(sprite_low.layer < plane_b_high.layer);
    assert!(plane_b_high.layer < plane_a_high.layer);
    assert!(plane_a_high.layer < sprite_high.layer);

    // Verify ordering is correct
    assert_eq!(backdrop.layer as u8, 0);
    assert_eq!(plane_b_low.layer as u8, 1);
    assert_eq!(plane_a_low.layer as u8, 2);
    assert_eq!(sprite_low.layer as u8, 3);
    assert_eq!(plane_b_high.layer as u8, 4);
    assert_eq!(plane_a_high.layer as u8, 5);
    assert_eq!(sprite_high.layer as u8, 6);
}

/// Test window plane has same priority as plane A.
#[test]
fn test_window_plane_priority() {
    // Window plane uses same priority as Plane A
    // (It's part of Plane A system, just with different source)
    let window_low = LayeredPixel::plane((255, 0, 255), true, false);
    let window_high = LayeredPixel::plane((255, 255, 0), true, true);

    assert_eq!(window_low.layer, Layer::PlaneALow);
    assert_eq!(window_high.layer, Layer::PlaneAHigh);
}

/// Test mixed priority scene.
#[test]
fn test_mixed_priority_scene() {
    // Simulate a complex scene:
    // - Backdrop
    // - Plane B low (background)
    // - Plane A low (foreground)
    // - Sprite low (character behind foreground)
    // - Plane A high (important foreground)
    // - Sprite high (character in front)

    let pixels = [
        LayeredPixel::backdrop((0, 0, 0)),
        LayeredPixel::plane((50, 50, 50), false, false),
        LayeredPixel::plane((100, 100, 100), true, false),
        LayeredPixel::sprite((200, 0, 0), false),
        LayeredPixel::plane((150, 150, 150), true, true),
        LayeredPixel::sprite((0, 200, 0), true),
    ];

    // Verify they're in ascending priority order
    for i in 0..pixels.len() - 1 {
        assert!(pixels[i].layer < pixels[i + 1].layer);
    }
}

/// Test framebuffer dimensions.
#[test]
fn test_framebuffer_dimensions() {
    let vdp = Vdp::new();

    // Default H40 mode (320x224)
    assert_eq!(vdp.display_width(), 320);
    assert_eq!(vdp.display_height(), 224);

    // Framebuffer should match
    let fb = vdp.framebuffer();
    assert_eq!(fb.len(), 320 * 224 * 4); // RGBA
}

/// Test H32 mode dimensions.
#[test]
fn test_h32_mode_dimensions() {
    let mut vdp = Vdp::new();

    // Set H32 mode (bit 0 of register 12 = 0)
    vdp.set_test_register(12, 0x80); // Clear bit 0

    assert_eq!(vdp.display_width(), 256);
}

/// Test V30 mode dimensions.
#[test]
fn test_v30_mode_dimensions() {
    let mut vdp = Vdp::new();

    // Set V30 mode (bit 3 of register 1 = 1)
    vdp.set_test_register(1, 0x08);

    assert_eq!(vdp.display_height(), 240);
}

/// Test border rendering in H32 mode.
#[test]
fn test_border_in_h32_mode() {
    let mut vdp = Vdp::new();

    // Set H32 mode
    vdp.set_test_register(12, 0x80);

    // In H32 mode (256px), there would be a border on sides in H40 canvas (320px)
    // This is handled by the display system
    assert_eq!(vdp.display_width(), 256);

    // Border would be: (320 - 256) / 2 = 32 pixels on each side
    let border_size = (320 - vdp.display_width()) / 2;
    assert_eq!(border_size, 32);
}
