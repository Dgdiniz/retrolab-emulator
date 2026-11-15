//! The sprites each line of the held frame held, and what became of each:
//! one registry of fates and one of line flags for every core.

/// What became of a sprite the evaluation of a line met: the values of
/// [`LineSprites::fates`]. A core never produces a fate its hardware lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SpriteFate {
    /// Drawn whole: every column of it on screen was drawn.
    DrawnWhole = 0,
    /// Drawn in part: the line ran out of time or dots inside it, and only
    /// the columns before were drawn.
    DrawnInPart = 1,
    /// Hidden by the console's sprite mask (the Mega Drive's sprite at x 0
    /// of the table hides every sprite after it on the line).
    Masked = 2,
    /// Past the line's sprite limit: met after the sprites the line keeps,
    /// dropped whole.
    OverLineLimit = 3,
    /// Left out: kept, or met, after the line ran out of time or dots, so no
    /// column of it was drawn.
    LeftOut = 4,
    /// Off screen: taken, its time or dots counted, but no column of it falls
    /// on the line's visible columns.
    OffScreen = 5,
}

/// The flags of a line in [`LineSprites::lines`]. One registry: no code has
/// two meanings, and a core never sets a flag its hardware lacks.
pub mod line_flags {
    /// The line met more sprites than it keeps (its sprite limit).
    pub const OVER_LIMIT: u16 = 1;
    /// The sprites met used up the line's time or dots.
    pub const OUT_OF_TIME: u16 = 2;
    /// The line above ran out of dots and carried the sprite mask into this
    /// one (the Mega Drive).
    pub const MASK_CARRIED_IN: u16 = 4;
    /// The line showed no sprite (the screen blanked, at brightness 0, or
    /// sprites shown on no screen); the evaluation ran all the same.
    pub const NOT_SHOWN: u16 = 8;
}

/// The values [`LineSprites::lines`] holds for each line.
pub const LINE_SPRITES_STRIDE: usize = 4;

/// The sprites each line of the held frame met, in the order its evaluation
/// met them, and what became of each ([`super::CaptureCore::line_sprites`]),
/// in columns: one entry per sprite a line met, the lines in the order they
/// were drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineSprites {
    /// The number of the frame the picture is, as a frame snapshot numbers
    /// it ([`super::FrameSnapshot::frame`]).
    pub frame: u64,
    /// The lines were evaluated from the settings the line table recorded
    /// for them; otherwise from the state as it stands.
    pub from_line_states: bool,
    /// [`LINE_SPRITES_STRIDE`] values a line: the line, as the core numbers
    /// its lines; the index of its first entry in the columns below; its
    /// number of entries; its flags ([`line_flags`]).
    pub lines: Vec<u16>,
    /// Per entry: the sprite's slot in the sprite table.
    pub slots: Vec<u8>,
    /// Per entry: what became of it ([`SpriteFate`]).
    pub fates: Vec<u8>,
    /// Per entry: its x on the screen, negative to the left of it.
    pub x: Vec<i16>,
    /// Per entry: its width in pixels.
    pub width: Vec<u8>,
    /// Per entry: the pixels of it the line drew on screen; 0 for a sprite
    /// not drawn.
    pub drawn_width: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fates_and_the_flags_keep_their_codes() {
        let fates = [
            SpriteFate::DrawnWhole,
            SpriteFate::DrawnInPart,
            SpriteFate::Masked,
            SpriteFate::OverLineLimit,
            SpriteFate::LeftOut,
            SpriteFate::OffScreen,
        ];
        assert_eq!(fates.map(|fate| fate as u8), [0, 1, 2, 3, 4, 5]);
        assert_eq!(
            [
                line_flags::OVER_LIMIT,
                line_flags::OUT_OF_TIME,
                line_flags::MASK_CARRIED_IN,
                line_flags::NOT_SHOWN
            ],
            [1, 2, 4, 8]
        );
        assert_eq!(LINE_SPRITES_STRIDE, 4);
    }
}
