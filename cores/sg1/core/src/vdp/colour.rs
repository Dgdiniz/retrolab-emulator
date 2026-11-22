//! The colours the VDP draws: the RGB a CRAM word shows, as it is,
//! shadowed and highlighted, and the levels a channel of it takes. The
//! renderer's tables of the palette (`cram_rgb` and its shadowed and
//! highlighted twins, all set by [`Vdp::set_palette_colour`]), the
//! debugger's palette readers and its exports all take their colours from
//! here, so an export shows a colour as the picture does.

use retro_core::video::ColourLevels;

use super::{Shade, Vdp};

/// The fifteen levels of a channel, darkest first: level k is k * 255 / 14,
/// rounded to the nearest.
const LEVELS: [u8; 15] = [
    0, 18, 36, 55, 73, 91, 109, 128, 146, 164, 182, 200, 219, 237, 255,
];

/// The levels a channel takes in every colour of the picture the VDP
/// draws, the debugger's views aside. A CRAM colour holds 3 bits a
/// channel, a value c of 0-7: as it is the channel shows level 2c,
/// shadowed level c, highlighted level c + 7 ([`Vdp::shaded_colour`]),
/// the scale shadow and highlight work on.
pub(super) const COLOUR_LEVELS: ColourLevels = ColourLevels {
    name: "mega-drive-15",
    levels: &LEVELS,
};

impl Vdp {
    /// The colour a CRAM word shows, as 8-bit red, green and blue. A word
    /// holds three bits of each, `0000 BBB0 GGG0 RRR0`; its other bits are
    /// ignored. A channel's value c shows level 2c of [`COLOUR_LEVELS`],
    /// which is c * 255 / 7 rounded to the nearest, the three bits repeated
    /// across the byte (`abc` becomes `abcabcab`): black is 0 and the
    /// brightest level 255.
    pub fn convert_genesis_color(color: u16) -> (u8, u8, u8) {
        Self::shaded_colour(color, Shade::AsIs)
    }

    /// The colour a CRAM word shows under `shade`: each channel's value c
    /// (0-7) at level 2c of [`COLOUR_LEVELS`] as it is, level c shadowed,
    /// level c + 7 highlighted. A shadowed colour is half the brightness,
    /// a highlighted one half again on the way to white, so a highlighted
    /// black is the middle grey and white stays white.
    pub(super) fn shaded_colour(color: u16, shade: Shade) -> (u8, u8, u8) {
        let level = |bits: u16| {
            let value = usize::from(bits & 7);
            LEVELS[match shade {
                Shade::AsIs => value * 2,
                Shade::Shadowed => value,
                Shade::Highlighted => value + 7,
            }]
        };
        (level(color >> 1), level(color >> 5), level(color >> 9))
    }

    /// Set palette entry `index` (0-63) to the colours CRAM word `word`
    /// shows: the renderer's table of them (`cram_rgb`), and beside it the
    /// same colours shadowed and highlighted, which the remap reads for a
    /// pixel shadow and highlight change. Every change of `cram_rgb` is made
    /// here, so the three tables always describe the same colours.
    #[inline]
    pub(super) fn set_palette_colour(&mut self, index: usize, word: u16) {
        self.cram_rgb[index] = Self::shaded_colour(word, Shade::AsIs);
        self.cram_rgb_shadowed[index] = Self::shaded_colour(word, Shade::Shadowed);
        self.cram_rgb_highlighted[index] = Self::shaded_colour(word, Shade::Highlighted);
    }

    /// Entry `index` of the palette as the remap colours a pixel of it
    /// under `shade`: `cram_rgb`'s, or its shadowed or highlighted twin's.
    #[inline(always)]
    pub(super) fn palette_colour(&self, index: usize, shade: Shade) -> (u8, u8, u8) {
        match shade {
            Shade::AsIs => self.cram_rgb[index],
            Shade::Shadowed => self.cram_rgb_shadowed[index],
            Shade::Highlighted => self.cram_rgb_highlighted[index],
        }
    }
}

#[cfg(test)]
mod tests {
    use retro_core::capture::CaptureRecording;
    use retro_core::video::VideoDebugger;

    use super::super::recording::colour_word;
    use super::*;

    #[test]
    fn test_color_conversion() {
        // Test black (000)
        let (r, g, b) = Vdp::convert_genesis_color(0x0000);
        assert_eq!((r, g, b), (0, 0, 0));

        // Test white (777) -> 0x0EEE
        let (r, g, b) = Vdp::convert_genesis_color(0x0EEE);
        assert_eq!((r, g, b), (255, 255, 255));

        // Test red (700) -> 0x000E
        let (r, g, b) = Vdp::convert_genesis_color(0x000E);
        assert_eq!(r, 255);
        assert_eq!(g, 0);
        assert_eq!(b, 0);

        // Test green (070) -> 0x00E0
        let (r, g, b) = Vdp::convert_genesis_color(0x00E0);
        assert_eq!(r, 0);
        assert_eq!(g, 255);
        assert_eq!(b, 0);

        // Test blue (007) -> 0x0E00
        let (r, g, b) = Vdp::convert_genesis_color(0x0E00);
        assert_eq!(r, 0);
        assert_eq!(g, 0);
        assert_eq!(b, 255);
    }

    #[test]
    fn the_colour_levels_are_the_fifteen_steps_of_shadow_and_highlight() {
        let levels = COLOUR_LEVELS;
        assert_eq!(levels.name, "mega-drive-15");
        assert_eq!(
            levels.levels,
            [0, 18, 36, 55, 73, 91, 109, 128, 146, 164, 182, 200, 219, 237, 255]
        );
        for (step, &level) in levels.levels.iter().enumerate() {
            let nearest = (step as f64 * 255.0 / 14.0).round() as u8;
            assert_eq!(level, nearest, "level {step}");
        }
        for value in 0..8u16 {
            let grey = (value << 1) | (value << 5) | (value << 9);
            let at = |step: u16| {
                let level = levels.levels[usize::from(step)];
                (level, level, level)
            };
            assert_eq!(Vdp::convert_genesis_color(grey), at(value * 2), "{value}");
            assert_eq!(Vdp::shaded_colour(grey, Shade::AsIs), at(value * 2));
            assert_eq!(Vdp::shaded_colour(grey, Shade::Shadowed), at(value));
            assert_eq!(Vdp::shaded_colour(grey, Shade::Highlighted), at(value + 7));
            // As it is, the three bits are repeated across the byte.
            let repeated = ((value << 5) | (value << 2) | (value >> 1)) as u8;
            assert_eq!(Vdp::convert_genesis_color(grey).0, repeated, "{value}");
        }
        assert_eq!(Vdp::new().colour_levels(), levels);
    }

    #[test]
    fn each_channel_of_a_colour_is_shaded_on_its_own() {
        // Red 7, green 2, blue 5.
        let word = (5 << 9) | (2 << 5) | (7 << 1);
        assert_eq!(Vdp::convert_genesis_color(word), (255, 73, 182));
        assert_eq!(Vdp::shaded_colour(word, Shade::Shadowed), (128, 36, 91));
        assert_eq!(
            Vdp::shaded_colour(word, Shade::Highlighted),
            (255, 164, 219)
        );
        // The bits outside the channels are ignored.
        assert_eq!(
            Vdp::shaded_colour(word | 0xF111, Shade::Highlighted),
            (255, 164, 219)
        );
    }

    /// Every colour a CRAM word holds, three bits each of red, green and
    /// blue: the 512 words `0000 BBB0 GGG0 RRR0`.
    fn every_colour_word() -> impl Iterator<Item = u16> {
        (0..512u16).map(|colour| {
            let (red, green, blue) = (colour & 7, (colour >> 3) & 7, colour >> 6);
            (blue << 9) | (green << 5) | (red << 1)
        })
    }

    /// Each of the 512 colours, written to CRAM through the data port as a
    /// game writes it, is exported as the renderer's table holds it: every
    /// pixel of a sprite of that colour, and the palette reader's entry.
    #[test]
    fn every_colour_is_exported_as_the_screen_shows_it() {
        let mut vdp = Vdp::new();
        vdp.registers[5] = 0; // the sprite table at 0
        vdp.registers[12] = 0; // H32
        vdp.registers[15] = 2; // the data port's address steps by a word

        // Sprite 0: one cell of pattern 1, palette line 0, at (0x80, 0x80).
        vdp.vram[0..8].copy_from_slice(&[0x00, 0x80, 0x00, 0x00, 0x00, 0x01, 0x00, 0x80]);
        // Pattern 1 (bytes 32-63): every pixel is colour 1.
        vdp.vram[32..64].fill(0x11);

        for word in every_colour_word() {
            vdp.code = 0x03; // a CRAM write
            vdp.address = 2; // colour 1 of line 0
            vdp.write_data_port(word, true);
            assert_eq!(vdp.cram[1], word);
            let (red, green, blue) = vdp.cram_rgb[1];

            let image = vdp.export_sprite_image(0).expect("sprite 0 exports");
            assert_eq!((image.width, image.height), (8, 8));
            for pixel in image.rgba.as_chunks::<4>().0 {
                assert_eq!(*pixel, [red, green, blue, 255], "word {word:#06X}");
            }
            let entry = vdp.get_palette_data()[0].colors[1];
            let shown = u32::from_be_bytes([red, green, blue, 0xFF]);
            assert_eq!(entry, shown, "word {word:#06X}");
        }
    }

    /// Whether the shadowed and highlighted tables hold, for every entry,
    /// the colours of the word `cram_rgb` holds: the three tables describe
    /// the same colours.
    fn assert_shades_follow_cram_rgb(vdp: &Vdp, when: &str) {
        for index in 0..64 {
            let word = colour_word(vdp.cram_rgb[index]);
            for shade in [Shade::AsIs, Shade::Shadowed, Shade::Highlighted] {
                assert_eq!(
                    vdp.palette_colour(index, shade),
                    Vdp::shaded_colour(word, shade),
                    "{when}: entry {index}, {shade:?}"
                );
            }
        }
    }

    /// The shadowed and highlighted colours are set with `cram_rgb` by
    /// every writer of CRAM, and stay with it through what leaves
    /// `cram_rgb` as it is: a new VDP, every write through the data port,
    /// a debugger write, a savestate's load, a reset (which clears CRAM,
    /// and the three tables with it) and a redraw of the frame.
    #[test]
    fn the_shaded_colours_follow_the_palette_wherever_it_changes() {
        let mut vdp = Vdp::new();
        assert_shades_follow_cram_rgb(&vdp, "a new VDP");
        assert_eq!(vdp.palette_colour(0, Shade::Highlighted), (128, 128, 128));

        vdp.registers[15] = 2;
        for (at, word) in every_colour_word().enumerate() {
            vdp.code = 0x03;
            vdp.address = ((at % 64) * 2) as u16;
            vdp.write_data_port(word, true);
            assert_shades_follow_cram_rgb(&vdp, "a data port write");
        }

        vdp.write_cram_debug(5, &[0x0246, 0x0ACE]);
        assert_eq!(vdp.palette_colour(5, Shade::Shadowed), (55, 36, 18));
        assert_shades_follow_cram_rgb(&vdp, "a debugger write");

        let state = vdp.to_savestate();
        let mut loaded = Vdp::new();
        loaded.from_savestate(&state);
        assert_eq!(loaded.cram_rgb, vdp.cram_rgb);
        assert_shades_follow_cram_rgb(&loaded, "a load");

        assert_ne!(vdp.cram_rgb, [(0, 0, 0); 64]);
        vdp.reset();
        assert_eq!(vdp.cram_rgb, [(0, 0, 0); 64], "a reset clears the table");
        assert_shades_follow_cram_rgb(&vdp, "a reset");

        // A frame drawn with its line states, colour 0 changed through the
        // data port part of the way down, then drawn again from the table:
        // each line installs the palette it was drawn with, and the live
        // tables are put back after.
        vdp.registers[1] = 0x44; // the display on
        vdp.registers[12] = 0x89; // 40 cells, shadow and highlight
        vdp.registers[15] = 2;
        vdp.set_capture_recording(CaptureRecording {
            line_states: true,
            ..CaptureRecording::default()
        });
        vdp.set_frame_being_drawn(1);
        for line in 0..224u16 {
            let word = match line {
                0 => 0x0E00,
                100 => 0x00E0,
                _ => {
                    vdp.scanline(line);
                    continue;
                }
            };
            vdp.code = 0x03;
            vdp.address = 0;
            vdp.write_data_port(word, true);
            vdp.scanline(line);
        }
        assert_eq!(vdp.cram_rgb[0], (0, 255, 0));
        let live = (
            vdp.cram_rgb,
            vdp.cram_rgb_shadowed,
            vdp.cram_rgb_highlighted,
        );
        assert!(vdp.rerender_current_frame(), "every line had its record");
        let after = (
            vdp.cram_rgb,
            vdp.cram_rgb_shadowed,
            vdp.cram_rgb_highlighted,
        );
        assert_eq!(after, live, "a redraw puts the three tables back");
        assert_shades_follow_cram_rgb(&vdp, "a redraw");
    }
}
