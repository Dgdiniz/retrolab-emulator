//! The PPU's plain registers: every write in `$2100-$2133` that is not
//! a memory port (VRAM, OAM, CGRAM) or Mode 7, and the `$213E` status
//! read. Nearly all of them unpack a byte into fields the renderer
//! reads; the three with a memory of their own are the scroll offsets
//! (two writes make a 10-bit value, through a latch shared by all of
//! them), COLDATA (one write updates whichever of the three colour
//! channels it selects) and INIDISP (a write on the first blanking
//! line while forced blank is on returns the OAM address to its base,
//! as the sprite evaluator would have).
//!
//! `$213C`, `$213D` and `$213F` never reach the PPU: the H/V latches
//! live in the system registers and the bus answers them itself.
//!
//! Written from fullsnes's PPU register map and Anomie's register
//! notes; checked against bsnes-plus.

use super::{window_layer, Ppu, PpuAccessCtx, WindowEnable};

/// PPU1's version, in the low bits of `$213E`. The first revision; the
/// one the reference traces come from.
const PPU1_VERSION: u8 = 1;

/// Bit `n` of a register byte, as a flag.
#[inline]
fn flag(value: u8, bit: u8) -> bool {
    value & (1 << bit) != 0
}

/// The field of `width` bits (1 to 8) that starts at bit `low`.
#[inline]
fn field(value: u8, low: u8, width: u8) -> u8 {
    debug_assert!((1..=8).contains(&width));
    (value >> low) & (0xff >> (8 - width))
}

/// Bits 0-3 as four flags, one per background.
#[inline]
fn low_flags(value: u8) -> [bool; 4] {
    core::array::from_fn(|i| flag(value, i as u8))
}

/// Bits 4-7 as four flags, one per background.
#[inline]
fn high_flags(value: u8) -> [bool; 4] {
    core::array::from_fn(|i| flag(value, 4 + i as u8))
}

/// Four two-bit fields, lowest first.
#[inline]
fn two_bit_fields(value: u8) -> [u8; 4] {
    core::array::from_fn(|i| (value >> (2 * i)) & 3)
}

/// A window selection nibble: enable and invert for each of the two
/// windows.
fn window_selection(nibble: u8) -> WindowEnable {
    WindowEnable {
        w1_invert: flag(nibble, 0),
        w1_enable: flag(nibble, 1),
        w2_invert: flag(nibble, 2),
        w2_enable: flag(nibble, 3),
    }
}

/// A 10-bit scroll offset from its new high byte and the two latches:
/// the previous write gives the low byte, but for a horizontal offset
/// the three lowest bits come from the write before that.
#[inline]
fn horizontal_offset(high: u8, previous: u8, before_that: u8) -> u16 {
    (u16::from(high) << 8) | u16::from(previous & !7) | u16::from(before_that & 7)
}

#[inline]
fn vertical_offset(high: u8, previous: u8) -> u16 {
    (u16::from(high) << 8) | u16::from(previous)
}

/// The fixed colour after a COLDATA write: bits 0-4 replace the
/// channels bits 5-7 select, red, green, blue.
fn updated_fixed_colour(colour: u16, value: u8) -> u16 {
    let intensity = u16::from(field(value, 0, 5));
    let mut colour = colour;
    for (channel, select) in [(0, 5), (5, 6), (10, 7)] {
        if flag(value, select) {
            colour = (colour & !(0x1f << channel)) | (intensity << channel);
        }
    }
    colour
}

impl Ppu {
    /// The first line of the vertical blank: 225, or 240 with overscan.
    pub(super) fn first_vblank_line(&self) -> u16 {
        if self.overscan {
            240
        } else {
            225
        }
    }

    /// `$2100` INIDISP: forced blank and the brightness. Written on the
    /// first blanking line while forced blank is on, it also returns the
    /// OAM address to its base.
    pub(super) fn write_2100(&mut self, value: u8, ctx: &PpuAccessCtx) {
        if self.display_disable && ctx.v_scanline == self.first_vblank_line() {
            self.oam_address_reset();
        }
        self.display_disable = flag(value, 7);
        self.display_brightness = field(value, 0, 4);
    }

    /// `$2101` OBSEL: the sprite sizes, the name select and the tile
    /// data base.
    pub(super) fn write_2101(&mut self, value: u8) {
        self.obj_base_size = field(value, 5, 3);
        self.obj_name_select = field(value, 3, 2);
        self.obj_tiledata_addr = u32::from(field(value, 0, 3)) << 14;
    }

    /// `$2105` BGMODE: the mode, BG3's priority and the tile sizes.
    pub(super) fn write_2105(&mut self, value: u8) {
        self.bgmode = field(value, 0, 3);
        self.bg3_priority = flag(value, 3);
        self.bg_tile_size = high_flags(value);
    }

    /// `$2106` MOSAIC: the block size (kept as the size itself, 1-16)
    /// and which backgrounds use it. Turning it on reloads the mosaic
    /// line counter; the renderer does not read that counter, it finds
    /// the mosaic rows from the scanline number.
    pub(super) fn write_2106(&mut self, value: u8) {
        let was_on = self.bg_mosaic_enable.iter().any(|&on| on);
        self.mosaic_size = field(value, 4, 4) + 1;
        if !was_on && field(value, 0, 4) != 0 {
            self.mosaic_vcounter = self.mosaic_size.saturating_add(1);
        }
        self.bg_mosaic_enable = low_flags(value);
    }

    /// `$2107-$210A` BGnSC: a background's tilemap base (in units of
    /// 1 KiB words) and size.
    fn write_bg_screen(&mut self, bg: usize, value: u8) {
        self.bg_screen_addr[bg] = u32::from(field(value, 2, 6)) << 11;
        self.bg_screen_size[bg] = field(value, 0, 2);
    }

    pub(super) fn write_2107(&mut self, value: u8) {
        self.write_bg_screen(0, value);
    }

    pub(super) fn write_2108(&mut self, value: u8) {
        self.write_bg_screen(1, value);
    }

    pub(super) fn write_2109(&mut self, value: u8) {
        self.write_bg_screen(2, value);
    }

    pub(super) fn write_210a(&mut self, value: u8) {
        self.write_bg_screen(3, value);
    }

    /// `$210B/$210C` BG12NBA, BG34NBA: two backgrounds' tile data bases
    /// (in units of 4 KiB words), a nibble each.
    fn write_bg_tiledata(&mut self, first: usize, value: u8) {
        self.bg_tiledata_addr[first] = u32::from(field(value, 0, 4)) << 13;
        self.bg_tiledata_addr[first + 1] = u32::from(field(value, 4, 4)) << 13;
    }

    pub(super) fn write_210b(&mut self, value: u8) {
        self.write_bg_tiledata(0, value);
    }

    pub(super) fn write_210c(&mut self, value: u8) {
        self.write_bg_tiledata(2, value);
    }

    /// `$210D-$2114` BGnHOFS/BGnVOFS: two writes per offset, through
    /// latches every offset register shares. A horizontal write updates
    /// both latches, a vertical one only the first.
    fn write_bg_hoffset(&mut self, bg: usize, value: u8) {
        self.bg_hoffset[bg] =
            horizontal_offset(value, self.bgofs_ppu1_latch, self.bgofs_ppu2_latch);
        self.bgofs_ppu1_latch = value;
        self.bgofs_ppu2_latch = value;
    }

    fn write_bg_voffset(&mut self, bg: usize, value: u8) {
        self.bg_voffset[bg] = vertical_offset(value, self.bgofs_ppu1_latch);
        self.bgofs_ppu1_latch = value;
    }

    /// BG1's offsets are also Mode 7's, through Mode 7's own latch.
    pub(super) fn write_210d(&mut self, value: u8) {
        self.mode7_hoffset = self.mode7_latched_word(value);
        self.write_bg_hoffset(0, value);
    }

    pub(super) fn write_210e(&mut self, value: u8) {
        self.mode7_voffset = self.mode7_latched_word(value);
        self.write_bg_voffset(0, value);
    }

    pub(super) fn write_210f(&mut self, value: u8) {
        self.write_bg_hoffset(1, value);
    }

    pub(super) fn write_2110(&mut self, value: u8) {
        self.write_bg_voffset(1, value);
    }

    pub(super) fn write_2111(&mut self, value: u8) {
        self.write_bg_hoffset(2, value);
    }

    pub(super) fn write_2112(&mut self, value: u8) {
        self.write_bg_voffset(2, value);
    }

    pub(super) fn write_2113(&mut self, value: u8) {
        self.write_bg_hoffset(3, value);
    }

    pub(super) fn write_2114(&mut self, value: u8) {
        self.write_bg_voffset(3, value);
    }

    /// `$2123-$2125` W12SEL, W34SEL, WOBJSEL: window selections for two
    /// layers each, a nibble per layer.
    fn write_window_selections(&mut self, low_layer: usize, high_layer: usize, value: u8) {
        self.window_enable[low_layer] = window_selection(field(value, 0, 4));
        self.window_enable[high_layer] = window_selection(field(value, 4, 4));
    }

    pub(super) fn write_2123(&mut self, value: u8) {
        self.write_window_selections(window_layer::BG1, window_layer::BG2, value);
    }

    pub(super) fn write_2124(&mut self, value: u8) {
        self.write_window_selections(window_layer::BG3, window_layer::BG4, value);
    }

    pub(super) fn write_2125(&mut self, value: u8) {
        self.write_window_selections(window_layer::OBJ, window_layer::COL, value);
    }

    /// `$2126-$2129` WH0-WH3: the two windows' edges.
    pub(super) fn write_2126(&mut self, value: u8) {
        self.window_one_left = value;
    }

    pub(super) fn write_2127(&mut self, value: u8) {
        self.window_one_right = value;
    }

    pub(super) fn write_2128(&mut self, value: u8) {
        self.window_two_left = value;
    }

    pub(super) fn write_2129(&mut self, value: u8) {
        self.window_two_right = value;
    }

    /// `$212A` WBGLOG: how the two windows combine, two bits per
    /// background (or, and, xor, xnor).
    pub(super) fn write_212a(&mut self, value: u8) {
        let [bg1, bg2, bg3, bg4] = two_bit_fields(value);
        self.window_combine[window_layer::BG1] = bg1;
        self.window_combine[window_layer::BG2] = bg2;
        self.window_combine[window_layer::BG3] = bg3;
        self.window_combine[window_layer::BG4] = bg4;
    }

    /// `$212B` WOBJLOG: the same for sprites and colour math.
    pub(super) fn write_212b(&mut self, value: u8) {
        let [obj, col, ..] = two_bit_fields(value);
        self.window_combine[window_layer::OBJ] = obj;
        self.window_combine[window_layer::COL] = col;
    }

    /// `$212C` TM: the layers on the main screen.
    pub(super) fn write_212c(&mut self, value: u8) {
        self.bg_main_enable = low_flags(value);
        self.obj_main_enable = flag(value, 4);
    }

    /// `$212D` TS: the layers on the sub screen.
    pub(super) fn write_212d(&mut self, value: u8) {
        self.bg_sub_enable = low_flags(value);
        self.obj_sub_enable = flag(value, 4);
    }

    /// `$212E` TMW: the layers the windows mask on the main screen.
    pub(super) fn write_212e(&mut self, value: u8) {
        self.bg_main_window_disable = low_flags(value);
        self.obj_main_window_disable = flag(value, 4);
    }

    /// `$212F` TSW: the same for the sub screen.
    pub(super) fn write_212f(&mut self, value: u8) {
        self.bg_sub_window_disable = low_flags(value);
        self.obj_sub_window_disable = flag(value, 4);
    }

    /// `$2130` CGWSEL: where the colour window blacks the main screen
    /// and where it stops colour math, whether the sub screen or the
    /// fixed colour is added, and direct colour.
    pub(super) fn write_2130(&mut self, value: u8) {
        self.col_main_mask = field(value, 6, 2);
        self.col_sub_mask = field(value, 4, 2);
        self.addsub_mode = flag(value, 1);
        self.direct_color = flag(value, 0);
    }

    /// `$2131` CGADSUB: subtract or add, halve, and which layers take
    /// part.
    pub(super) fn write_2131(&mut self, value: u8) {
        self.color_math_subtract = flag(value, 7);
        self.color_math_halve = flag(value, 6);
        self.color_math_back_enable = flag(value, 5);
        self.color_math_oam_enable = flag(value, 4);
        self.color_math_bg_enable = low_flags(value);
    }

    /// `$2132` COLDATA.
    pub(super) fn write_2132(&mut self, value: u8) {
        self.fixed_color = updated_fixed_colour(self.fixed_color, value);
    }

    /// `$2133` SETINI: Mode 7's extra background, pseudo-hires,
    /// overscan, and the two interlace bits.
    pub(super) fn write_2133(&mut self, value: u8) {
        self.mode7_extbg = flag(value, 6);
        self.pseudo_hires = flag(value, 3);
        self.overscan = flag(value, 2);
        self.obj_interlace = flag(value, 1);
        self.interlace = flag(value, 0);
    }

    /// `$213E` STAT77: the sprite overflow flags over PPU1's version;
    /// bit 4 is not driven and keeps the bus's value.
    pub(super) fn read_213e(&mut self, ctx: &PpuAccessCtx) -> u8 {
        let value = (ctx.mdr & 0x10)
            | (u8::from(self.sprite_time_over) << 7)
            | (u8::from(self.sprite_range_over) << 6)
            | (PPU1_VERSION & 0x0f);
        self.ppu1_mdr = value;
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> PpuAccessCtx {
        PpuAccessCtx::ZERO
    }

    #[test]
    fn a_field_is_the_bits_from_low_for_width() {
        assert_eq!(field(0b1110_0101, 5, 3), 0b111);
        assert_eq!(field(0b1110_0101, 0, 3), 0b101);
        assert_eq!(field(0b1110_0101, 2, 2), 0b01);
        assert_eq!(field(0xff, 0, 8), 0xff);
    }

    #[test]
    fn inidisp_splits_forced_blank_and_brightness() {
        let mut ppu = Ppu::new();
        ppu.write_2100(0x8f, &ctx());
        assert!(ppu.display_disable);
        assert_eq!(ppu.display_brightness, 0x0f);
        ppu.write_2100(0x05, &ctx());
        assert!(!ppu.display_disable);
        assert_eq!(ppu.display_brightness, 0x05);
    }

    #[test]
    fn leaving_forced_blank_on_the_first_blanking_line_resets_the_oam_address() {
        let mut ppu = Ppu::new();
        ppu.write_2102(0x08); // base 0x10
        ppu.oam_addr = 0x100;
        let line_225 = PpuAccessCtx {
            mdr: 0,
            h_dot: 0,
            v_scanline: 225,
        };
        ppu.write_2100(0x0f, &line_225);
        assert_eq!(ppu.oam_addr, 0x10);
        // Not on another line, and not while the display is on.
        ppu.oam_addr = 0x100;
        ppu.write_2100(0x8f, &ctx());
        assert_eq!(ppu.oam_addr, 0x100);
        ppu.write_2100(0x0f, &line_225);
        assert_eq!(ppu.oam_addr, 0x10);
        ppu.oam_addr = 0x100;
        ppu.write_2100(0x0f, &line_225);
        assert_eq!(ppu.oam_addr, 0x100, "the display was already on");
        // With overscan the first blanking line is 240.
        ppu.overscan = true;
        assert_eq!(ppu.first_vblank_line(), 240);
    }

    #[test]
    fn obsel_unpacks_its_three_fields() {
        let mut ppu = Ppu::new();
        ppu.write_2101(0xe7);
        assert_eq!(ppu.obj_base_size, 7);
        assert_eq!(ppu.obj_name_select, 0);
        assert_eq!(ppu.obj_tiledata_addr, 7 << 14);
        ppu.write_2101(0x18);
        assert_eq!(ppu.obj_name_select, 3);
    }

    #[test]
    fn bgmode_unpacks_the_mode_and_the_tile_sizes() {
        let mut ppu = Ppu::new();
        ppu.write_2105(0xfb);
        assert_eq!(ppu.bgmode, 3);
        assert!(ppu.bg3_priority);
        assert_eq!(ppu.bg_tile_size, [true; 4]);
        ppu.write_2105(0x20);
        assert_eq!(ppu.bgmode, 0);
        assert!(!ppu.bg3_priority);
        assert_eq!(ppu.bg_tile_size, [false, true, false, false]);
    }

    #[test]
    fn mosaic_keeps_the_size_itself_and_starts_the_counter_when_turned_on() {
        let mut ppu = Ppu::new();
        ppu.write_2106(0x4f);
        assert_eq!(ppu.mosaic_size, 5);
        assert_eq!(ppu.bg_mosaic_enable, [true; 4]);
        assert_eq!(ppu.mosaic_vcounter, 6);
        // Already on: the counter is left alone.
        ppu.mosaic_vcounter = 2;
        ppu.write_2106(0x71);
        assert_eq!(ppu.mosaic_size, 8);
        assert_eq!(ppu.mosaic_vcounter, 2);
        assert_eq!(ppu.bg_mosaic_enable, [true, false, false, false]);
    }

    #[test]
    fn bgnsc_unpacks_the_tilemap_base_and_size() {
        let mut ppu = Ppu::new();
        ppu.write_2107(0xfe);
        assert_eq!(ppu.bg_screen_addr[0], 0x1f800);
        assert_eq!(ppu.bg_screen_size[0], 2);
        ppu.write_210a(0x05);
        assert_eq!(ppu.bg_screen_addr[3], 0x0800);
        assert_eq!(ppu.bg_screen_size[3], 1);
    }

    #[test]
    fn bgnnba_gives_two_backgrounds_their_tile_data_bases() {
        let mut ppu = Ppu::new();
        ppu.write_210b(0xab);
        assert_eq!(ppu.bg_tiledata_addr[0], 0xb << 13);
        assert_eq!(ppu.bg_tiledata_addr[1], 0xa0 << 9);
        ppu.write_210c(0x21);
        assert_eq!(ppu.bg_tiledata_addr[2], 0x1 << 13);
        assert_eq!(ppu.bg_tiledata_addr[3], 0x20 << 9);
    }

    #[test]
    fn a_horizontal_offset_takes_its_low_bits_from_two_writes_back() {
        let mut ppu = Ppu::new();
        ppu.write_210d(0x12);
        assert_eq!(ppu.bg_hoffset[0], 0x1200);
        assert_eq!((ppu.bgofs_ppu1_latch, ppu.bgofs_ppu2_latch), (0x12, 0x12));
        ppu.write_210d(0x34);
        assert_eq!(ppu.bg_hoffset[0], 0x3400 | 0x10 | 0x02);
    }

    #[test]
    fn a_vertical_write_updates_only_the_first_latch() {
        let mut ppu = Ppu::new();
        ppu.write_210e(0x55);
        assert_eq!(ppu.bg_voffset[0], 0x5500);
        assert_eq!((ppu.bgofs_ppu1_latch, ppu.bgofs_ppu2_latch), (0x55, 0));
        ppu.write_210d(0x10);
        assert_eq!(ppu.bg_hoffset[0], 0x1000 | 0x50);
        // The latches are shared by every background.
        ppu.write_2113(0x20);
        assert_eq!(ppu.bg_hoffset[3], 0x2000 | 0x10);
    }

    #[test]
    fn bg1s_offsets_are_mode_7s_too_through_their_own_latch() {
        let mut ppu = Ppu::new();
        ppu.write_210d(0x77);
        assert_eq!(ppu.mode7_hoffset, 0x7700);
        assert_eq!(ppu.mode7_latchdata, 0x77);
        ppu.write_210e(0x88);
        assert_eq!(ppu.mode7_voffset, 0x8877);
        assert_eq!(ppu.mode7_latchdata, 0x88);
        let before = ppu.mode7_hoffset;
        ppu.write_210f(0x22);
        assert_eq!(ppu.mode7_hoffset, before, "BG2's offset is not Mode 7's");
    }

    #[test]
    fn window_selections_unpack_a_nibble_per_layer() {
        let mut ppu = Ppu::new();
        ppu.write_2123(0xa5);
        assert_eq!(
            ppu.window_enable[window_layer::BG1],
            WindowEnable {
                w1_invert: true,
                w1_enable: false,
                w2_invert: true,
                w2_enable: false,
            }
        );
        assert_eq!(
            ppu.window_enable[window_layer::BG2],
            WindowEnable {
                w1_invert: false,
                w1_enable: true,
                w2_invert: false,
                w2_enable: true,
            }
        );
        ppu.write_2125(0x0f);
        assert_eq!(
            ppu.window_enable[window_layer::OBJ],
            WindowEnable {
                w1_invert: true,
                w1_enable: true,
                w2_invert: true,
                w2_enable: true,
            }
        );
        assert_eq!(
            ppu.window_enable[window_layer::COL],
            WindowEnable::default()
        );
    }

    #[test]
    fn the_window_edges_and_combine_logic_are_kept_per_layer() {
        let mut ppu = Ppu::new();
        ppu.write_2126(1);
        ppu.write_2127(2);
        ppu.write_2128(3);
        ppu.write_2129(4);
        assert_eq!(
            (
                ppu.window_one_left,
                ppu.window_one_right,
                ppu.window_two_left,
                ppu.window_two_right
            ),
            (1, 2, 3, 4)
        );
        ppu.write_212a(0b11_10_01_00);
        assert_eq!(&ppu.window_combine[..4], &[0, 1, 2, 3]);
        ppu.write_212b(0b0000_0110);
        assert_eq!(ppu.window_combine[window_layer::OBJ], 2);
        assert_eq!(ppu.window_combine[window_layer::COL], 1);
    }

    #[test]
    fn the_screen_and_window_masks_are_five_flags_each() {
        let mut ppu = Ppu::new();
        ppu.write_212c(0x15);
        assert_eq!(ppu.bg_main_enable, [true, false, true, false]);
        assert!(ppu.obj_main_enable);
        ppu.write_212d(0x02);
        assert_eq!(ppu.bg_sub_enable, [false, true, false, false]);
        assert!(!ppu.obj_sub_enable);
        ppu.write_212e(0x18);
        assert_eq!(ppu.bg_main_window_disable, [false, false, false, true]);
        assert!(ppu.obj_main_window_disable);
        ppu.write_212f(0x01);
        assert_eq!(ppu.bg_sub_window_disable, [true, false, false, false]);
        assert!(!ppu.obj_sub_window_disable);
    }

    #[test]
    fn cgwsel_and_cgadsub_unpack_the_colour_math_setup() {
        let mut ppu = Ppu::new();
        ppu.write_2130(0b10_01_00_11);
        assert_eq!((ppu.col_main_mask, ppu.col_sub_mask), (2, 1));
        assert!(ppu.addsub_mode);
        assert!(ppu.direct_color);
        ppu.write_2131(0xc5);
        assert!(ppu.color_math_subtract);
        assert!(ppu.color_math_halve);
        assert!(!ppu.color_math_back_enable);
        assert!(!ppu.color_math_oam_enable);
        assert_eq!(ppu.color_math_bg_enable, [true, false, true, false]);
    }

    #[test]
    fn coldata_updates_only_the_channels_it_selects() {
        let mut ppu = Ppu::new();
        ppu.write_2132(0x3f);
        assert_eq!(ppu.fixed_color, 0x001f);
        ppu.write_2132(0x50);
        assert_eq!(ppu.fixed_color, 0x001f | (0x10 << 5));
        ppu.write_2132(0x85);
        assert_eq!(ppu.fixed_color, 0x001f | (0x10 << 5) | (0x05 << 10));
        ppu.write_2132(0xa0);
        assert_eq!(ppu.fixed_color, 0x10 << 5);
        assert_eq!(updated_fixed_colour(0x7fff, 0xe0), 0);
        assert_eq!(updated_fixed_colour(0, 0x1f), 0, "no channel selected");
    }

    #[test]
    fn setini_unpacks_its_five_bits() {
        let mut ppu = Ppu::new();
        ppu.write_2133(0x47);
        assert!(ppu.mode7_extbg);
        assert!(ppu.overscan);
        assert!(ppu.obj_interlace);
        assert!(ppu.interlace);
        assert!(!ppu.pseudo_hires);
        ppu.write_2133(0x08);
        assert!(ppu.pseudo_hires);
        assert!(!ppu.overscan);
    }

    #[test]
    fn stat77_reports_the_overflow_flags_over_the_version() {
        let mut ppu = Ppu::new();
        let bus = PpuAccessCtx {
            mdr: 0x10,
            h_dot: 0,
            v_scanline: 0,
        };
        ppu.sprite_time_over = true;
        ppu.sprite_range_over = false;
        assert_eq!(ppu.read_213e(&bus), 0x91);
        assert_eq!(ppu.ppu1_mdr, 0x91);
        ppu.sprite_range_over = true;
        assert_eq!(ppu.read_213e(&PpuAccessCtx::ZERO), 0xc1);
    }
}
