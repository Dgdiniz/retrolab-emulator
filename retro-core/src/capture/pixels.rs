//! Where each pixel of the held frame came from: the planes a host asks for,
//! the shared part of their packing, and the trace a renderer is generic
//! over so that the frame loops draw with no trace in their code.

use std::fmt;
use std::marker::PhantomData;

/// The planes a pixel-sources pass fills, as the bits of the mask a host
/// passes ([`super::CaptureCore::pixel_sources`]).
pub mod planes {
    /// The `layer` plane: what drew each pixel.
    pub const LAYER: u32 = 1;
    /// The `colour` plane: the colour each pixel read, in the core's own
    /// encoding.
    pub const COLOUR: u32 = 2;
    /// The `tile` plane: the character each pixel was read from
    /// ([`super::tile_value`]).
    pub const TILE: u32 = 4;
    /// The `cell` plane: the map entry or the sprite of each pixel.
    pub const CELL: u32 = 8;
    /// The four planes of the main picture, what a host gets when it names
    /// none.
    pub const MAIN: u32 = LAYER | COLOUR | TILE | CELL;
    /// The planes of a second screen a core composites with (the SNES sub
    /// screen): `sub_layer` and `sub_colour`.
    pub const SUB_SCREEN: u32 = 16;
}

/// The values of the `layer` plane's bits 0-2, what drew the pixel, which
/// every core shares: 0 the backdrop, a layer's position in the core's layer
/// list plus one ([`super::drawn_by`]), 6 a colour no layer drew, 7 nothing.
/// Bits 3-15 are the core's own, documented by the core.
pub mod layer {
    /// The bits naming what drew the pixel, bits 0-2.
    pub const WHAT_DREW: u16 = 0x0007;
    /// The backdrop drew the pixel.
    pub const BACKDROP: u16 = 0;
    /// A colour no layer drew: one the console showed straight from what it
    /// moved, as the Mega Drive shows a palette transfer running with the
    /// display off.
    pub const UNLAYERED_COLOUR: u16 = 6;
    /// Nothing drew the pixel: the line was drawn blank, or what drew it is
    /// not known (a line the line table holds no record of).
    pub const NOTHING: u16 = 7;
}

/// The `layer` plane's bits 0-2 for a pixel the layer at position `layer`
/// of the core's layer list drew (the ids the debugger's layer toggles use):
/// its position plus one, so that 0 stays the backdrop.
#[inline]
pub const fn drawn_by(layer: u16) -> u16 {
    layer + 1
}

/// The `cell` plane's value for a pixel that came from no map entry and no
/// sprite.
pub const NO_CELL: u16 = 0xFFFF;

/// The `tile` plane's value for a pixel read from a character starting at
/// video memory word `start_word`, number `character`, at row `row` and
/// column `column` of it, both after flips:
///
/// | Bits | Meaning |
/// |---|---|
/// | 0-14 | the word where the character starts (`start_word`, its low 15 bits) |
/// | 15-25 | the character number (`character`, its low 11 bits) |
/// | 26-28 | the row read in the character (`row`, its low 3 bits) |
/// | 29-31 | the column read in the character (`column`, its low 3 bits) |
///
/// 0 is the value of a pixel no character drew.
#[inline]
pub const fn tile_value(start_word: u32, character: u32, row: u32, column: u32) -> u32 {
    (start_word & 0x7FFF) | ((character & 0x07FF) << 15) | ((row & 7) << 26) | ((column & 7) << 29)
}

/// Where each pixel of the held frame came from
/// ([`super::CaptureCore::pixel_sources`]): one value a pixel in each plane
/// asked for, row after row, `width` by `height`, cut as the picture is.
///
/// - `layer`: bits 0-2 what drew the pixel ([`layer`]); bits 3-15 the core's
///   own.
/// - `colour`: the colour the pixel read, in the core's own encoding.
/// - `tile`: the character the pixel was read from ([`tile_value`]); 0 where
///   no character drew it.
/// - `cell`: a map pixel's entry, as the index of the video memory word
///   that holds it; a sprite pixel's slot in the sprite table; [`NO_CELL`]
///   otherwise. A core may set bits of its own beside them (the SNES marks a
///   Mode 7 pixel outside its plane).
/// - `sub_layer` and `sub_colour`: what a second screen the core composites
///   with holds at the pixel, with the codes of `layer`'s bits 0-2 and the
///   core's colour encoding; `None` on a core without one, or when not asked
///   for ([`planes::SUB_SCREEN`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelSources {
    /// The number of the frame the picture is, as a frame snapshot numbers
    /// it ([`super::FrameSnapshot::frame`]).
    pub frame: u64,
    /// The picture's width in pixels.
    pub width: u32,
    /// The picture's height in pixels.
    pub height: u32,
    /// The picture was drawn again from the line table, each line from the
    /// settings it was drawn with.
    pub from_line_states: bool,
    /// The picture drawn again with the trace is the picture the host holds,
    /// byte for byte: the planes describe that picture.
    pub matches_picture: bool,
    /// What drew each pixel; `None` when not asked for.
    pub layer: Option<Vec<u16>>,
    /// The colour each pixel read; `None` when not asked for.
    pub colour: Option<Vec<u16>>,
    /// The character each pixel was read from; `None` when not asked for.
    pub tile: Option<Vec<u32>>,
    /// The map entry or sprite each pixel came from; `None` when not asked
    /// for.
    pub cell: Option<Vec<u16>>,
    /// What the second screen holds at each pixel; `None` when the core has
    /// none or it was not asked for.
    pub sub_layer: Option<Vec<u8>>,
    /// The second screen's colour at each pixel; `None` when the core has
    /// none or it was not asked for.
    pub sub_colour: Option<Vec<u16>>,
}

/// What a renderer notes, as it draws each pixel, of where the pixel came
/// from. The renderer is generic over it: every note sits behind
/// `if T::ON`, so drawn with [`NoTrace`] (as the frame loops draw) the notes
/// are compiled out of the renderer, and only a traced redraw takes them.
pub trait PixelTrace {
    /// Where the notes go: the core's own record of a traced line or frame.
    type Notes;
    /// Whether this trace notes anything.
    const ON: bool;
    /// The notes. Called only when [`Self::ON`] is true.
    fn notes(&mut self) -> &mut Self::Notes;
}

/// The trace the frame loops draw with: it notes nothing, and takes no room.
/// `N` is the notes type of the core's traced draw, so that one renderer
/// accepts both.
pub struct NoTrace<N>(PhantomData<fn() -> N>);

impl<N> NoTrace<N> {
    /// The trace that notes nothing.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<N> Default for NoTrace<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N> Clone for NoTrace<N> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<N> Copy for NoTrace<N> {}

impl<N> fmt::Debug for NoTrace<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NoTrace")
    }
}

impl<N> PixelTrace for NoTrace<N> {
    type Notes = N;
    const ON: bool = false;

    fn notes(&mut self) -> &mut N {
        unreachable!("a draw with no trace takes no notes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_planes_are_one_bit_each_and_main_is_the_first_four() {
        assert_eq!(
            [
                planes::LAYER,
                planes::COLOUR,
                planes::TILE,
                planes::CELL,
                planes::SUB_SCREEN
            ],
            [1, 2, 4, 8, 16]
        );
        assert_eq!(planes::MAIN, 15);
    }

    #[test]
    fn a_layer_draws_as_its_position_plus_one() {
        assert_eq!(drawn_by(0), 1);
        assert_eq!(drawn_by(4), 5);
        assert_ne!(drawn_by(0), layer::BACKDROP);
        assert_eq!(layer::WHAT_DREW, 7);
        assert_eq!((layer::UNLAYERED_COLOUR, layer::NOTHING), (6, 7));
        assert_eq!(layer::NOTHING & layer::WHAT_DREW, layer::NOTHING);
    }

    #[test]
    fn the_tile_value_packs_the_word_the_character_the_row_and_the_column() {
        assert_eq!(tile_value(0x7FFF, 0x7FF, 7, 7), 0xFFFF_FFFF);
        assert_eq!(
            tile_value(0x1234, 0x155, 3, 5),
            0x1234 | (0x155 << 15) | (3 << 26) | (5 << 29)
        );
        assert_eq!(tile_value(0, 0, 0, 0), 0);
        // Each field keeps only its own bits.
        assert_eq!(tile_value(0x8000, 0x800, 8, 8), 0);
        // A character of 16 words: its start is its number times 16, which
        // keeps 11 bits of the number in the 15 bits of the word.
        let pattern = 0x7FFu32;
        let value = tile_value(pattern * 16, pattern, 5, 6);
        assert_eq!(value & 0x7FFF, 0x7FF0);
        assert_eq!((value >> 15) & 0x7FF, 0x7FF);
        assert_eq!((value >> 26) & 7, 5);
        assert_eq!((value >> 29) & 7, 6);
    }

    /// A traced draw's notes, for the trace tests.
    #[derive(Default)]
    struct Notes {
        pixels: Vec<u16>,
    }

    impl PixelTrace for Notes {
        type Notes = Notes;
        const ON: bool = true;

        fn notes(&mut self) -> &mut Notes {
            self
        }
    }

    /// A renderer generic over its trace, as a core's is: it draws `pixels`
    /// and notes each one only when the trace is on.
    fn draw<T: PixelTrace<Notes = Notes>>(pixels: &[u16], trace: &mut T) -> u32 {
        let mut drawn = 0;
        for &pixel in pixels {
            drawn += u32::from(pixel);
            if T::ON {
                trace.notes().pixels.push(pixel);
            }
        }
        drawn
    }

    #[test]
    fn a_draw_with_no_trace_notes_nothing_and_a_traced_one_notes_every_pixel() {
        assert_eq!(std::mem::size_of::<NoTrace<Notes>>(), 0);
        let mut untraced = NoTrace::<Notes>::default();
        assert_eq!(draw(&[1, 2, 3], &mut untraced), 6);
        let mut traced = Notes::default();
        assert_eq!(draw(&[1, 2, 3], &mut traced), 6);
        assert_eq!(traced.pixels, [1, 2, 3]);
    }

    #[test]
    #[should_panic(expected = "a draw with no trace takes no notes")]
    fn asking_no_trace_for_its_notes_is_a_mistake() {
        NoTrace::<Notes>::new().notes();
    }
}
