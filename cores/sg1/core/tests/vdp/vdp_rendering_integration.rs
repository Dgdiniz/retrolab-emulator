//! The picture as the renderer the console runs composes it: the backdrop,
//! the order of the planes and sprites, and the frame's dimensions.

use super::draw::*;
use sg1_core::vdp::Vdp;

#[test]
fn an_empty_screen_shows_the_backdrop_colour() {
    let mut vdp = vdp();
    set_colour(&mut vdp, 1, RED);
    vdp.set_test_register(7, 0x01); // the backdrop: palette 0, colour 1
    assert_eq!(vdp.registers()[7], 0x01);
    draw(&mut vdp);
    assert_eq!(pixel(&vdp, 0, 0), shown(RED));
    assert_eq!(pixel(&vdp, 319, 223), shown(RED));
}

/// What can draw a pixel, from back to front: the backdrop, the two planes
/// and the sprites, each low and high priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Backdrop,
    PlaneBLow,
    PlaneALow,
    SpriteLow,
    PlaneBHigh,
    PlaneAHigh,
    SpriteHigh,
}

impl Source {
    /// From back to front.
    const ALL: [Source; 7] = [
        Source::Backdrop,
        Source::PlaneBLow,
        Source::PlaneALow,
        Source::SpriteLow,
        Source::PlaneBHigh,
        Source::PlaneAHigh,
        Source::SpriteHigh,
    ];

    /// Its colour index, and the tile of that colour it draws with.
    fn colour(self) -> u16 {
        Source::ALL.iter().position(|&s| s == self).unwrap() as u16 + 1
    }

    /// The plane or sprite it is: two sources of one pair can never be the
    /// same one.
    fn place(self) -> u8 {
        match self {
            Source::Backdrop => 0,
            Source::PlaneBLow | Source::PlaneBHigh => 1,
            Source::PlaneALow | Source::PlaneAHigh => 2,
            Source::SpriteLow | Source::SpriteHigh => 3,
        }
    }

    /// Put it at the top left of the screen.
    fn put(self, vdp: &mut Vdp) {
        let tile = self.colour();
        let high = matches!(
            self,
            Source::PlaneBHigh | Source::PlaneAHigh | Source::SpriteHigh
        );
        let value = entry(tile, 0, false, false, high);
        match self.place() {
            0 => vdp.set_test_register(7, tile as u8),
            1 => put_cell(vdp, PLANE_B, 32, 0, 0, value),
            2 => put_cell(vdp, PLANE_A, 32, 0, 0, value),
            _ => put_sprite(vdp, 0, (0, 0), (1, 1), 0, value),
        }
    }
}

#[test]
fn every_pair_of_sources_shows_the_one_nearer_the_front() {
    let colours = [RED, GREEN, BLUE, WHITE, 0x0EE0, 0x00EE, 0x0E0E];
    for (i, &back) in Source::ALL.iter().enumerate() {
        for &front in &Source::ALL[i + 1..] {
            if back.place() == front.place() {
                continue;
            }
            let mut vdp = vdp();
            for (k, &word) in colours.iter().enumerate() {
                let colour = k as u16 + 1;
                put_tile(&mut vdp, colour, &solid_tile(colour as u8));
                set_colour(&mut vdp, u32::from(colour), word);
            }
            // The backdrop stays colour 0 unless it is one of the pair.
            back.put(&mut vdp);
            front.put(&mut vdp);
            draw(&mut vdp);
            let expected = shown(colours[front.colour() as usize - 1]);
            assert_eq!(pixel(&vdp, 0, 0), expected, "{front:?} over {back:?}");
        }
    }
}

#[test]
fn the_framebuffer_holds_320_by_224() {
    let vdp = Vdp::new();
    assert_eq!(vdp.display_width(), 320);
    assert_eq!(vdp.display_height(), 224);
    assert_eq!(vdp.framebuffer().len(), 320 * 224 * 4);
}

#[test]
fn the_32_cell_mode_is_256_wide() {
    let mut vdp = Vdp::new();
    vdp.set_test_register(12, 0x80);
    assert_eq!(vdp.display_width(), 256);
}

#[test]
fn the_30_cell_mode_is_240_high() {
    let mut vdp = Vdp::new();
    vdp.set_test_register(1, 0x08);
    assert_eq!(vdp.display_height(), 240);
}

#[test]
fn the_32_cell_mode_leaves_a_border_on_each_side_of_the_40_cell_frame() {
    let mut vdp = Vdp::new();
    vdp.set_test_register(12, 0x80);
    assert_eq!(vdp.display_width(), 256);
    let border_size = (320 - vdp.display_width()) / 2;
    assert_eq!(border_size, 32);
}
