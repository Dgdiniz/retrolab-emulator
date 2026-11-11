//! The projection commands: a camera over a flat ground.
//!
//! `Parameter` places the camera. The game names a point of the ground it
//! looks at (the focus), how far back from it the eye is and how far the
//! screen is in front of the eye, and two angles, the azimuth it looks
//! along and the zenith it tilts by. The chip keeps what the other three
//! commands need:
//!
//! - `Raster` gives, for one line of the screen, the four numbers of the
//!   Mode 7 matrix that draw the ground as seen on that line;
//! - `Project` takes a point in space to its place on the screen and its
//!   size there;
//! - `Target` takes a place on the screen back to the ground under it.
//!
//! All of it is perspective by similar triangles: a line of the screen is as
//! far below the horizon as the ground it shows is near, so the ground one
//! screen unit covers is the eye's height over that distance.

use super::number::{dot, moved_down, q15, q15_wide, split_wide, Scaled, Turn};
use super::rom;

/// The steepest zenith the chip lets the camera take, by the eye's height:
/// one entry for each number of spare bits in the height. Nearer the
/// horizon than this, `Parameter` holds the angle and makes up the
/// difference (see [`Camera::aim`]).
const STEEPEST_ZENITH: [i16; 16] = [
    0x38b4, 0x38b7, 0x38ba, 0x38be, 0x38c0, 0x38c4, 0x38c7, 0x38ca, 0x38ce, 0x38d0, 0x38d4, 0x38d7,
    0x38da, 0x38dd, 0x38e0, 0x38e4,
];

/// Words of camera state a savestate carries.
pub(super) const WORDS: usize = 24;

/// What `Parameter` leaves behind for the other projection commands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Camera {
    /// The azimuth the camera looks along.
    azimuth: Turn,
    /// The zenith as the game gave it.
    zenith: Turn,
    /// The unit vector from the focus toward the eye.
    axis: [i16; 3],
    /// The point of the ground at the middle of the view.
    ground: [i16; 2],
    /// The middle of the screen, in space.
    screen: [i16; 3],
    /// The eye's distance to the screen, normalised and as given.
    to_screen: Scaled,
    to_screen_word: i16,
    /// The eye's height over the ground.
    height: Scaled,
    /// The zenith held at its steepest, which is what the screen's lines
    /// are measured against.
    tilt: Turn,
    /// `1 / cos(tilt)` before and after the cosine is corrected for the
    /// part of the zenith that was held back.
    secant_held: Scaled,
    secant: Scaled,
    /// How far below the screen's middle the horizon falls.
    horizon: i16,
}

impl Camera {
    /// The state in the order a savestate keeps it.
    pub(super) fn words(&self) -> [i16; WORDS] {
        [
            self.azimuth.sin,
            self.azimuth.cos,
            self.zenith.sin,
            self.zenith.cos,
            self.axis[0],
            self.axis[1],
            self.axis[2],
            self.ground[0],
            self.ground[1],
            self.screen[0],
            self.screen[1],
            self.screen[2],
            self.to_screen.mantissa,
            self.to_screen.exponent,
            self.to_screen_word,
            self.height.mantissa,
            self.height.exponent,
            self.tilt.sin,
            self.tilt.cos,
            self.secant_held.mantissa,
            self.secant_held.exponent,
            self.secant.mantissa,
            self.secant.exponent,
            self.horizon,
        ]
    }

    /// The state back from [`Camera::words`].
    pub(super) fn from_words(w: &[i16; WORDS]) -> Self {
        let turn = |at: usize| Turn {
            sin: w[at],
            cos: w[at + 1],
        };
        let scaled = |at: usize| Scaled {
            mantissa: w[at],
            exponent: w[at + 1],
        };
        Self {
            azimuth: turn(0),
            zenith: turn(2),
            axis: [w[4], w[5], w[6]],
            ground: [w[7], w[8]],
            screen: [w[9], w[10], w[11]],
            to_screen: scaled(12),
            to_screen_word: w[14],
            height: scaled(15),
            tilt: turn(17),
            secant_held: scaled(19),
            secant: scaled(21),
            horizon: w[23],
        }
    }

    /// `Parameter`: place the camera. Returns the raster offset of the
    /// screen's middle, the raster line of the vanishing point, and the
    /// ground point at the middle of the view.
    pub(super) fn aim(
        &mut self,
        focus: [i16; 3],
        eye_distance: i16,
        screen_distance: i16,
        azimuth: i16,
        zenith: i16,
    ) -> [i16; 4] {
        self.azimuth = Turn::of(azimuth);
        self.zenith = Turn::of(zenith);
        self.axis = [
            q15_wide(i32::from(self.zenith.sin), -i32::from(self.azimuth.sin)),
            q15(self.zenith.sin, self.azimuth.cos),
            q15(self.zenith.cos, 0x7fff),
        ];

        // The eye sits back from the focus along the axis; the screen is in
        // front of the eye, back toward the focus.
        let axis = self.axis;
        let along = |distance: i16| axis.map(|part| q15(distance, part));
        let back = along(eye_distance);
        let forward = along(screen_distance);
        let eye = [0, 1, 2].map(|i| focus[i].wrapping_add(back[i]));
        self.screen = [0, 1, 2].map(|i| eye[i].wrapping_sub(forward[i]));
        self.to_screen = Scaled::normalized(screen_distance, 0);
        self.to_screen_word = screen_distance;
        self.height = Scaled::normalized(eye[2], 0);

        // Hold the zenith at the steepest the height allows.
        let spare = (-self.height.exponent).clamp(0, 15) as usize;
        let steepest = if zenith < 0 {
            -STEEPEST_ZENITH[spare]
        } else {
            STEEPEST_ZENITH[spare]
        };
        let held = if zenith < 0 {
            zenith.max(steepest + 1)
        } else {
            zenith.min(steepest)
        };
        self.tilt = Turn::of(held);
        self.secant_held = Scaled {
            mantissa: self.tilt.cos,
            exponent: 0,
        }
        .reciprocal();

        // The view's axis meets the ground height x tan(tilt) away from the
        // point under the eye, along the azimuth.
        let run = q15(
            self.height.times(self.secant_held).renormalized().to_word(),
            self.tilt.sin,
        );
        self.ground = [
            eye[0].wrapping_add(q15(run, self.azimuth.sin)),
            eye[1].wrapping_sub(q15(run, self.azimuth.cos)),
        ];

        // Past the steepest zenith the chip stands in for the angle it held
        // back with two short series in it: one moves the screen's middle
        // down the raster, the other stretches the cosine.
        let mut raster_offset: i16 = 0;
        if zenith != held || zenith == steepest {
            let asked = zenith.max(-32767);
            let mut past = asked.wrapping_sub(steepest);
            if past >= 0 {
                past -= 1;
            }
            let x = !(i32::from(past) << 2) as i16;
            let x_squared = q15(x, x);

            let slope = q15(q15(x, rom_word(rom::CLIP_OFFSET_CUBIC)), x)
                .wrapping_add(rom_word(rom::CLIP_OFFSET_LINEAR));
            raster_offset = raster_offset.wrapping_sub(q15(q15(slope, x), screen_distance));

            let stretch = q15(x_squared, rom_word(rom::CLIP_COSINE_QUARTIC))
                .wrapping_add(rom_word(rom::CLIP_COSINE_SQUARE));
            self.tilt.cos = self
                .tilt
                .cos
                .wrapping_add(q15(q15(x_squared, stretch), self.tilt.cos));
        }

        self.horizon = q15(screen_distance, self.tilt.cos);

        // The vanishing point is the horizon's drop over sin(tilt) above the
        // screen's middle.
        let cosecant = Scaled {
            mantissa: self.tilt.sin,
            exponent: 0,
        }
        .reciprocal();
        let drop = Scaled::normalized(self.horizon, cosecant.exponent);
        let mut rise = Scaled::normalized(q15(drop.mantissa, cosecant.mantissa), drop.exponent);
        if rise.mantissa == i16::MIN {
            // -1 cannot change sign in a word: halve it first.
            rise = Scaled {
                mantissa: i16::MIN >> 1,
                exponent: rise.exponent.wrapping_add(1),
            };
        }
        let vanishing_line = Scaled {
            mantissa: rise.mantissa.wrapping_neg(),
            exponent: rise.exponent,
        }
        .to_word();

        self.secant = Scaled {
            mantissa: self.tilt.cos,
            exponent: 0,
        }
        .reciprocal();

        [
            raster_offset,
            vanishing_line,
            self.ground[0],
            self.ground[1],
        ]
    }

    /// The ground one screen unit covers on raster `line`: the eye's height
    /// over the line's distance below the horizon. `unit` is the power of
    /// two the answer is counted in.
    fn ground_per_unit(&self, line: i16, unit: i16) -> Scaled {
        let below_horizon = Scaled {
            mantissa: q15(line, self.zenith.sin).wrapping_add(self.horizon),
            exponent: unit,
        };
        below_horizon.reciprocal().times(self.height)
    }

    /// `Raster`: the Mode 7 matrix `[A, B, C, D]` for one line. Across the
    /// screen the ground runs at the line's scale; down the screen it runs
    /// faster by the tilt's secant; both are turned by the azimuth.
    pub(super) fn raster(&self, line: i16) -> [i16; 4] {
        let scale = self.ground_per_unit(line, 7);
        let across = scale.renormalized().to_word();
        let down = scale.times(self.secant).renormalized().to_word();
        [
            q15(across, self.azimuth.cos),
            q15_wide(i32::from(down), -i32::from(self.azimuth.sin)),
            q15(across, self.azimuth.sin),
            q15(down, self.azimuth.cos),
        ]
    }

    /// `Target`: the ground point under the screen position `(h, v)`.
    pub(super) fn target(&self, h: i16, v: i16) -> [i16; 2] {
        let scale = self.ground_per_unit(v, 8);
        let across = q15(scale.renormalized().to_word(), (i32::from(h) << 8) as i16);
        let down = q15(
            scale.times(self.secant_held).renormalized().to_word(),
            (i32::from(v) << 8) as i16,
        );
        [
            self.ground[0]
                .wrapping_add(q15(across, self.azimuth.cos))
                .wrapping_add(q15_wide(i32::from(down), -i32::from(self.azimuth.sin))),
            self.ground[1]
                .wrapping_sub(q15(across, self.azimuth.sin))
                .wrapping_add(q15(down, self.azimuth.cos)),
        ]
    }

    /// `Project`: where the point `at` falls on the screen, `[H, V]`, and
    /// the size it is drawn at there, `M`.
    ///
    /// The point is taken from the screen's middle and measured along three
    /// directions: the view's axis, which gives its distance from the eye,
    /// and the screen's own right and up, which give its place. The place
    /// and the size shrink with the distance: each is multiplied by the
    /// screen's distance over the point's.
    pub(super) fn project(&self, at: [i16; 3]) -> [i16; 3] {
        // The offset's three parts, each normalised on its own and halved
        // for headroom, then brought to the smallest of the three scales.
        let parts = [0, 1, 2].map(|i| {
            let (mantissa, places) = split_wide(i32::from(at[i]) - i32::from(self.screen[i]));
            (mantissa >> 1, places - 1)
        });
        let places = parts.iter().fold(i16::MAX, |least, part| least.min(part.1));
        let offset = parts.map(|(mantissa, own)| moved_down(mantissa, own - places));

        // The eye's distance to the point: the screen's, plus how far the
        // point lies beyond the screen along the axis.
        let unit = 16 - places;
        let beyond = i32::from(dot(offset, self.axis).wrapping_neg());
        let beyond = if unit >= 0 {
            beyond.wrapping_shl(unit as u32)
        } else {
            beyond.wrapping_shr(unit.unsigned_abs().into())
        };
        // The chip drops a lone -1 here before it halves the value.
        let beyond = if beyond == -1 { 0 } else { beyond };
        let beyond = beyond >> 1;
        let (distance, distance_places) =
            split_wide(i32::from(self.to_screen_word as u16).wrapping_add(beyond));
        let distance_exponent = 15 - distance_places;

        let nearness = Scaled {
            mantissa: distance,
            exponent: 0,
        }
        .reciprocal();
        let shrink = q15(nearness.mantissa, self.to_screen.mantissa);
        let exponent = self.to_screen.exponent.wrapping_sub(distance_exponent);

        let right = [
            q15(self.azimuth.cos, 0x7fff),
            q15(self.azimuth.sin, 0x7fff),
            0,
        ];
        let up = [
            q15_wide(i32::from(self.zenith.cos), -i32::from(self.azimuth.sin)),
            q15(self.zenith.cos, self.azimuth.cos),
            q15_wide(-i32::from(self.zenith.sin), 0x7fff),
        ];
        let on_screen = |direction: [i16; 3]| {
            Scaled::normalized(
                q15(dot(offset, direction), shrink),
                exponent.wrapping_add(unit),
            )
            .to_word()
        };

        let size = Scaled::normalized(
            shrink,
            nearness.exponent.wrapping_add(exponent).wrapping_sub(7),
        )
        .to_word();
        [on_screen(right), on_screen(up), size]
    }
}

/// One ROM word as a 1.15 number.
fn rom_word(address: i32) -> i16 {
    rom::word(address) as i16
}
