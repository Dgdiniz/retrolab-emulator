//! The CGRAM port: `$2121` CGADD, `$2122` CGDATA, `$213B` CGDATAREAD.
//!
//! CGRAM holds the 256 colours, fifteen bits each (five of blue, green
//! and red, the top bit always clear), and the CPU reaches it a byte at
//! a time: CGADD names a colour and the running address counts bytes
//! from there. A colour is written in two halves, the low byte waiting
//! in a latch until the high byte arrives with it; the high byte's top
//! bit is dropped. A read returns the half the address names; on the
//! high half the top bit is whatever the bus last carried, since the
//! chip never drives it.
//!
//! While the picture is being drawn, from dot 22 to dot 273 of lines 1
//! to the last visible one, the colour-math fetcher owns the port, and
//! a CPU access goes where it is reading instead of to the running
//! address; the renderer reads its palette entries straight from the
//! array, so that internal address stays at 0.
//!
//! Written from fullsnes's CGRAM port description; the drawing window
//! checked against bsnes-plus.

use super::{Ppu, PpuAccessCtx};

/// The dots of a drawn line during which the fetcher owns the port.
const FETCHER_DOTS: core::ops::Range<u16> = 22..274;

impl Ppu {
    /// Whether the CPU's accesses are redirected to the fetcher's
    /// address: line 1 to the last visible one, within the fetcher's
    /// dots, while the picture is being drawn.
    pub(super) fn cgram_in_active_scan(&self, dot: u16, line: u16) -> bool {
        !self.display_disable
            && line != 0
            && self.in_active_scanline(line)
            && FETCHER_DOTS.contains(&dot)
    }

    /// The byte address an access goes to now.
    fn cgram_target(&self, ctx: &PpuAccessCtx) -> u16 {
        if self.cgram_in_active_scan(ctx.h_dot, ctx.v_scanline) {
            self.cgram_iaddr & 0x1ff
        } else {
            self.cgram_addr & 0x1ff
        }
    }

    /// Move the running address on; it wraps at nine bits.
    #[inline]
    fn step_cgram_address(&mut self) {
        self.cgram_addr = (self.cgram_addr + 1) & 0x1ff;
    }

    /// One half of a colour: the low byte at an even address.
    pub(super) fn cgram_read_byte(&self, address: u16) -> u8 {
        let [low, high] = self.cgram[usize::from(address >> 1) & 0xff].to_le_bytes();
        if address & 1 == 0 {
            low
        } else {
            high
        }
    }

    /// `$2121` CGADD: the colour the running address starts at.
    pub(super) fn write_2121(&mut self, value: u8) {
        self.cgram_addr = u16::from(value) << 1;
    }

    /// `$2122` CGDATA.
    pub(super) fn write_2122(&mut self, value: u8, ctx: &PpuAccessCtx) {
        let odd = self.cgram_addr & 1 != 0;
        let target = self.cgram_target(ctx);
        self.step_cgram_address();
        if odd {
            let colour = usize::from(target >> 1) & 0xff;
            self.cgram[colour] = u16::from_le_bytes([self.cgram_latchdata, value & 0x7f]);
        } else {
            self.cgram_latchdata = value;
        }
    }

    /// `$213B` CGDATAREAD.
    pub(super) fn read_213b(&mut self, ctx: &PpuAccessCtx) -> u8 {
        let odd = self.cgram_addr & 1 != 0;
        let target = self.cgram_target(ctx);
        let half = self.cgram_read_byte(target);
        let value = if odd {
            (self.ppu2_mdr & 0x80) | (half & 0x7f)
        } else {
            half
        };
        self.ppu2_mdr = value;
        self.step_cgram_address();
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context for a chip in forced blank (the reset state): no
    /// redirection.
    fn blank_ctx() -> PpuAccessCtx {
        PpuAccessCtx::ZERO
    }

    #[test]
    fn cgadd_names_a_colour_and_the_address_counts_bytes() {
        let mut ppu = Ppu::new();
        ppu.write_2121(0x10);
        assert_eq!(ppu.cgram_addr, 0x20);
    }

    #[test]
    fn a_colour_is_written_in_two_halves_with_the_top_bit_dropped() {
        let mut ppu = Ppu::new();
        let ctx = blank_ctx();
        ppu.write_2121(0x05);
        ppu.write_2122(0x34, &ctx);
        assert_eq!(ppu.cgram_latchdata, 0x34);
        assert_eq!(ppu.cgram[5], 0x0000, "the low byte waits");
        assert_eq!(ppu.cgram_addr, 0x0b);
        ppu.write_2122(0xfe, &ctx);
        assert_eq!(ppu.cgram[5], 0x7e34);
        assert_eq!(ppu.cgram_addr, 0x0c);
        for colour in 0..3u8 {
            ppu.write_2121(colour);
            ppu.write_2122(0xaa, &ctx);
            ppu.write_2122(0xff, &ctx);
            assert_eq!(ppu.cgram[usize::from(colour)], 0x7faa);
        }
    }

    #[test]
    fn a_read_returns_the_half_with_the_bus_bit_on_top() {
        let mut ppu = Ppu::new();
        let ctx = blank_ctx();
        ppu.cgram[3] = 0x7fab;
        ppu.write_2121(0x03);
        assert_eq!(ppu.read_213b(&ctx), 0xab);
        assert_eq!(ppu.cgram_addr, 0x07);
        ppu.ppu2_mdr = 0x80;
        assert_eq!(ppu.read_213b(&ctx), 0xff);
        assert_eq!(ppu.ppu2_mdr, 0xff);
        assert_eq!(ppu.cgram_addr, 0x08);
        ppu.ppu2_mdr = 0x00;
        ppu.cgram_addr = 0x07;
        assert_eq!(ppu.read_213b(&ctx), 0x7f);
    }

    #[test]
    fn the_running_address_wraps_at_nine_bits() {
        let mut ppu = Ppu::new();
        let ctx = blank_ctx();
        ppu.cgram_addr = 0x1ff;
        ppu.write_2122(0x00, &ctx);
        assert_eq!(ppu.cgram_addr, 0x000);
        assert_eq!(ppu.cgram_read_byte(0x3ff), ppu.cgram_read_byte(0x1ff));
    }

    #[test]
    fn the_fetcher_owns_the_port_inside_its_window() {
        let mut ppu = Ppu::new();
        assert!(!ppu.cgram_in_active_scan(150, 50), "forced blank: never");
        ppu.display_disable = false;
        assert!(!ppu.cgram_in_active_scan(150, 0));
        assert!(ppu.cgram_in_active_scan(150, 224));
        assert!(!ppu.cgram_in_active_scan(150, 225));
        assert!(!ppu.cgram_in_active_scan(21, 50));
        assert!(ppu.cgram_in_active_scan(22, 50));
        assert!(ppu.cgram_in_active_scan(273, 50));
        assert!(!ppu.cgram_in_active_scan(274, 50));
        ppu.overscan = true;
        assert!(ppu.cgram_in_active_scan(150, 239));
        assert!(!ppu.cgram_in_active_scan(150, 240));
    }

    #[test]
    fn inside_the_window_the_access_goes_where_the_fetcher_is() {
        let mut ppu = Ppu::new();
        ppu.display_disable = false;
        let drawing = PpuAccessCtx {
            mdr: 0,
            h_dot: 100,
            v_scanline: 50,
        };
        ppu.cgram[0] = 0x1234;
        ppu.cgram_iaddr = 0;
        ppu.write_2121(0x40);
        assert_eq!(ppu.read_213b(&drawing), 0x34, "colour 0, not colour 64");
        assert_eq!(ppu.cgram_addr, 0x81, "the running address moves anyway");
        ppu.cgram_addr = 0x80;
        ppu.write_2122(0xcd, &drawing);
        ppu.write_2122(0xab, &drawing);
        assert_eq!(ppu.cgram[0], 0x2bcd, "written where the fetcher reads");
        assert_eq!(ppu.cgram[0x40], 0);
    }
}
