//! The colours the VDP draws: the RGB a CRAM word shows, and the levels a
//! channel of it takes. The renderer's table of the palette (`cram_rgb`),
//! the debugger's palette readers and its exports all take their colours
//! from [`Vdp::convert_genesis_color`], so an export shows a colour as the
//! picture does.

use retro_core::video::ColourLevels;

use super::Vdp;

/// The eight levels a channel takes in every colour the picture draws
/// without shadow or highlight: a CRAM colour's 3 bits a channel, each
/// level its bits repeated across the byte ([`Vdp::convert_genesis_color`]).
/// Shadow and highlight are worked out from a colour on these levels,
/// halved or raised by half and capped ([`Vdp::shadow_color`],
/// [`Vdp::highlight_color`]), and need not land on one.
pub(super) const COLOUR_LEVELS: ColourLevels = ColourLevels {
    name: "mega-drive-8",
    levels: &[0, 36, 73, 109, 146, 182, 219, 255],
};

impl Vdp {
    /// The colour a CRAM word shows, as 8-bit red, green and blue. A word
    /// holds three bits of each, `0000 BBB0 GGG0 RRR0`; its other bits are
    /// ignored. Each channel's three bits are repeated across the byte
    /// (`abc` becomes `abcabcab`), which is the level times 255 / 7 rounded
    /// to the nearest: black is 0 and the brightest level 255
    /// ([`COLOUR_LEVELS`]).
    pub fn convert_genesis_color(color: u16) -> (u8, u8, u8) {
        let r = ((color >> 1) & 0x7) as u8;
        let g = ((color >> 5) & 0x7) as u8;
        let b = ((color >> 9) & 0x7) as u8;

        let r8 = (r << 5) | (r << 2) | (r >> 1);
        let g8 = (g << 5) | (g << 2) | (g >> 1);
        let b8 = (b << 5) | (b << 2) | (b >> 1);

        (r8, g8, b8)
    }
}

#[cfg(test)]
mod tests {
    use retro_core::video::VideoDebugger;

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
    fn the_colour_levels_are_those_a_cram_channel_converts_to() {
        let levels = COLOUR_LEVELS;
        assert_eq!(levels.name, "mega-drive-8");
        assert_eq!(levels.levels.len(), 8);
        for (channel, &level) in levels.levels.iter().enumerate() {
            let bits = channel as u16;
            let grey = (bits << 1) | (bits << 5) | (bits << 9);
            assert_eq!(
                Vdp::convert_genesis_color(grey),
                (level, level, level),
                "channel value {channel}"
            );
            let nearest = (f64::from(bits) * 255.0 / 7.0).round() as u8;
            assert_eq!(level, nearest, "channel value {channel}");
        }
        assert_eq!(Vdp::new().colour_levels(), levels);
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
}
