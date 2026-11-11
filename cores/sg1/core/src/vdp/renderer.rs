//! VDP rendering types.
//!
//! Defines layer priority types used for rendering compositing.

/// Layer type for priority system.
///
/// Layers are ordered from back (lowest) to front (highest).
/// During rendering, higher layers overwrite lower layers when non-transparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Backdrop = 0,
    PlaneBLow = 1,
    PlaneALow = 2,
    SpriteLow = 3,
    PlaneBHigh = 4,
    PlaneAHigh = 5,
    SpriteHigh = 6,
}

/// Pixel with layer and priority information.
///
/// Used for layer compositing during scanline rendering.
#[derive(Debug, Clone, Copy)]
pub struct LayeredPixel {
    pub color: (u8, u8, u8),
    pub layer: Layer,
    pub transparent: bool,
}

impl LayeredPixel {
    /// Create a transparent pixel.
    pub fn transparent() -> Self {
        Self {
            color: (0, 0, 0),
            layer: Layer::Backdrop,
            transparent: true,
        }
    }

    /// Create a backdrop pixel.
    pub fn backdrop(color: (u8, u8, u8)) -> Self {
        Self {
            color,
            layer: Layer::Backdrop,
            transparent: false,
        }
    }

    /// Create a plane pixel.
    pub fn plane(color: (u8, u8, u8), is_plane_a: bool, high_priority: bool) -> Self {
        let layer = match (is_plane_a, high_priority) {
            (true, true) => Layer::PlaneAHigh,
            (true, false) => Layer::PlaneALow,
            (false, true) => Layer::PlaneBHigh,
            (false, false) => Layer::PlaneBLow,
        };
        Self {
            color,
            layer,
            transparent: false,
        }
    }

    /// Create a sprite pixel.
    pub fn sprite(color: (u8, u8, u8), high_priority: bool) -> Self {
        let layer = if high_priority {
            Layer::SpriteHigh
        } else {
            Layer::SpriteLow
        };
        Self {
            color,
            layer,
            transparent: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layer_priority_order() {
        assert!(Layer::Backdrop < Layer::PlaneBLow);
        assert!(Layer::PlaneBLow < Layer::PlaneALow);
        assert!(Layer::PlaneALow < Layer::SpriteLow);
        assert!(Layer::SpriteLow < Layer::PlaneBHigh);
        assert!(Layer::PlaneBHigh < Layer::PlaneAHigh);
        assert!(Layer::PlaneAHigh < Layer::SpriteHigh);
    }

    #[test]
    fn test_layered_pixel_transparent() {
        let pixel = LayeredPixel::transparent();
        assert!(pixel.transparent);
        assert_eq!(pixel.layer, Layer::Backdrop);
    }

    #[test]
    fn test_layered_pixel_backdrop() {
        let pixel = LayeredPixel::backdrop((255, 0, 0));
        assert!(!pixel.transparent);
        assert_eq!(pixel.layer, Layer::Backdrop);
        assert_eq!(pixel.color, (255, 0, 0));
    }

    #[test]
    fn test_layered_pixel_plane_a_low() {
        let pixel = LayeredPixel::plane((0, 255, 0), true, false);
        assert!(!pixel.transparent);
        assert_eq!(pixel.layer, Layer::PlaneALow);
        assert_eq!(pixel.color, (0, 255, 0));
    }

    #[test]
    fn test_layered_pixel_plane_a_high() {
        let pixel = LayeredPixel::plane((0, 0, 255), true, true);
        assert!(!pixel.transparent);
        assert_eq!(pixel.layer, Layer::PlaneAHigh);
        assert_eq!(pixel.color, (0, 0, 255));
    }

    #[test]
    fn test_layered_pixel_plane_b_low() {
        let pixel = LayeredPixel::plane((255, 255, 0), false, false);
        assert!(!pixel.transparent);
        assert_eq!(pixel.layer, Layer::PlaneBLow);
    }

    #[test]
    fn test_layered_pixel_plane_b_high() {
        let pixel = LayeredPixel::plane((255, 0, 255), false, true);
        assert!(!pixel.transparent);
        assert_eq!(pixel.layer, Layer::PlaneBHigh);
    }

    #[test]
    fn test_layered_pixel_sprite_low() {
        let pixel = LayeredPixel::sprite((0, 255, 255), false);
        assert!(!pixel.transparent);
        assert_eq!(pixel.layer, Layer::SpriteLow);
    }

    #[test]
    fn test_layered_pixel_sprite_high() {
        let pixel = LayeredPixel::sprite((128, 128, 128), true);
        assert!(!pixel.transparent);
        assert_eq!(pixel.layer, Layer::SpriteHigh);
    }

    #[test]
    fn test_priority_comparison() {
        let backdrop = LayeredPixel::backdrop((0, 0, 0));
        let plane_b_low = LayeredPixel::plane((255, 0, 0), false, false);
        let sprite_high = LayeredPixel::sprite((0, 255, 0), true);

        assert!(backdrop.layer < plane_b_low.layer);
        assert!(plane_b_low.layer < sprite_high.layer);
    }
}
