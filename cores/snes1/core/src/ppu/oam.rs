//! The OAM port: `$2102/$2103` OAMADD, `$2104` OAMDATA, `$2138`
//! OAMDATAREAD.
//!
//! OAM is 544 bytes: a low table of 128 four-byte sprite entries (X, Y,
//! tile, attributes) and a high table of 32 bytes holding, two bits per
//! sprite, the ninth X bit and the size flag. The CPU's address is ten
//! bits wide, so the high table repeats sixteen times across the upper
//! half of the address space.
//!
//! OAMADD sets a base address in words; a write to either half returns
//! the running address to it. The base also picks the sprite the
//! evaluator starts from when OAMADDH's top bit asks for priority
//! rotation, which puts that sprite first in the priority order.
//!
//! Low-table writes go in pairs: the even byte waits in a latch and the
//! odd byte stores both. High-table writes store at once. Every access
//! moves the running address on, but while the picture is being drawn
//! the access itself goes where the sprite evaluator is reading, not
//! where the CPU's address points.
//!
//! Written from fullsnes's OAM port description and Anomie's notes on
//! the priority rotation; checked against bsnes-plus.

use super::{Ppu, PpuAccessCtx};

/// Bytes in the low table; the high table follows.
const LOW_TABLE_LEN: u16 = 0x200;

/// Where a ten-bit address lands in the 544 bytes: the high table's
/// 32 bytes repeat across `$200-$3ff`.
#[inline]
fn oam_slot(address: u16) -> u16 {
    let address = address & 0x3ff;
    if address >= LOW_TABLE_LEN {
        LOW_TABLE_LEN | (address & 0x1f)
    } else {
        address
    }
}

impl Ppu {
    /// Return the running addresses to the base, and pick the sprite the
    /// evaluator starts from: sprite 0, or the base's sprite under
    /// priority rotation.
    pub(super) fn oam_address_reset(&mut self) {
        self.oam_addr = self.oam_baseaddr;
        self.oam_iaddr = self.oam_baseaddr;
        self.oam_first_sprite = if self.oam_priority {
            ((self.oam_baseaddr >> 2) & 0x7f) as u8
        } else {
            0
        };
    }

    /// The reset the chip performs as sprite evaluation ends at the top
    /// of the vertical blank, only when the picture was being drawn, as
    /// forced blank stops the evaluator too. Driven by the `VBlankStart`
    /// event.
    pub(crate) fn oam_reset_at_vblank(&mut self) {
        if !self.display_disable {
            self.oam_address_reset();
        }
    }

    /// Whether the CPU's accesses are redirected to the evaluator's
    /// address: while the picture is being drawn.
    fn oam_redirected(&self, line: u16) -> bool {
        !self.display_disable && self.in_active_scanline(line)
    }

    /// The ten-bit address an access goes to now.
    fn oam_target(&self, ctx: &PpuAccessCtx) -> u16 {
        if self.oam_redirected(ctx.v_scanline) {
            self.oam_iaddr
        } else {
            self.oam_addr
        }
    }

    /// Move the running address on; it wraps at ten bits.
    #[inline]
    fn step_oam_address(&mut self) {
        self.oam_addr = self.oam_addr.wrapping_add(1) & 0x3ff;
    }

    /// The byte at a ten-bit address.
    pub(super) fn oam_read_byte(&self, address: u16) -> u8 {
        self.oam[usize::from(oam_slot(address))]
    }

    pub(super) fn oam_write_byte(&mut self, address: u16, value: u8) {
        self.oam[usize::from(oam_slot(address))] = value;
    }

    /// `$2102` OAMADDL: the base's low byte, in words.
    pub(super) fn write_2102(&mut self, value: u8) {
        self.oam_baseaddr = (self.oam_baseaddr & 0x0600) | (u16::from(value) << 1);
        self.oam_address_reset();
    }

    /// `$2103` OAMADDH: the base's top bit, and the priority rotation.
    pub(super) fn write_2103(&mut self, value: u8) {
        self.oam_priority = value & 0x80 != 0;
        self.oam_baseaddr = (u16::from(value & 1) << 9) | (self.oam_baseaddr & 0x01fe);
        self.oam_address_reset();
    }

    /// `$2104` OAMDATA.
    pub(super) fn write_2104(&mut self, value: u8, ctx: &PpuAccessCtx) {
        let odd = self.oam_addr & 1 != 0;
        let target = self.oam_target(ctx);
        self.step_oam_address();
        if !odd {
            self.oam_latchdata = value;
        }
        if target & LOW_TABLE_LEN != 0 {
            self.oam_write_byte(target, value);
        } else if odd {
            let first = target & !1;
            self.oam_write_byte(first, self.oam_latchdata);
            self.oam_write_byte(first + 1, value);
        }
    }

    /// `$2138` OAMDATAREAD: the byte at the running address (or where
    /// the evaluator is), which then moves on.
    pub(super) fn read_2138(&mut self, ctx: &PpuAccessCtx) -> u8 {
        let value = self.oam_read_byte(self.oam_target(ctx));
        self.step_oam_address();
        self.ppu1_mdr = value;
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context for a chip in forced blank (the reset state): no
    /// redirection whatever the line.
    fn blank_ctx() -> PpuAccessCtx {
        PpuAccessCtx {
            mdr: 0,
            h_dot: 0,
            v_scanline: 50,
        }
    }

    #[test]
    fn the_high_table_repeats_across_the_upper_half() {
        assert_eq!(oam_slot(0x000), 0x000);
        assert_eq!(oam_slot(0x1ff), 0x1ff);
        assert_eq!(oam_slot(0x200), 0x200);
        assert_eq!(oam_slot(0x21f), 0x21f);
        assert_eq!(oam_slot(0x220), 0x200);
        assert_eq!(oam_slot(0x250), 0x210);
        assert_eq!(oam_slot(0x3ff), 0x21f);
        assert_eq!(oam_slot(0x7ff), 0x21f, "eleventh bit ignored");
    }

    #[test]
    fn the_base_is_set_in_words_and_resets_the_running_addresses() {
        let mut ppu = Ppu::new();
        ppu.write_2102(0x10);
        assert_eq!(ppu.oam_baseaddr, 0x20);
        assert_eq!((ppu.oam_addr, ppu.oam_iaddr), (0x20, 0x20));
        ppu.write_2103(0x01);
        assert_eq!(ppu.oam_baseaddr, 0x220);
        assert_eq!(ppu.oam_addr, 0x220);
    }

    #[test]
    fn priority_rotation_starts_the_evaluator_at_the_base_sprite() {
        let mut ppu = Ppu::new();
        ppu.write_2102(0x64); // base 0xc8: sprite 50
        ppu.write_2103(0x00);
        assert_eq!(ppu.oam_first_sprite, 0);
        ppu.write_2103(0x80);
        assert!(ppu.oam_priority);
        assert_eq!(ppu.oam_first_sprite, 50);
        ppu.write_2103(0x00);
        assert_eq!(ppu.oam_first_sprite, 0);
    }

    #[test]
    fn low_table_writes_land_in_pairs() {
        let mut ppu = Ppu::new();
        let ctx = blank_ctx();
        ppu.write_2102(0x00);
        ppu.write_2103(0x00);
        ppu.write_2104(0xaa, &ctx);
        assert_eq!(ppu.oam[0], 0, "the even byte waits");
        assert_eq!(ppu.oam_latchdata, 0xaa);
        assert_eq!(ppu.oam_addr, 1);
        ppu.write_2104(0xbb, &ctx);
        assert_eq!(&ppu.oam[0..2], &[0xaa, 0xbb]);
        assert_eq!(ppu.oam_addr, 2);
    }

    #[test]
    fn high_table_writes_land_at_once_and_follow_the_mirror() {
        let mut ppu = Ppu::new();
        let ctx = blank_ctx();
        ppu.write_2103(0x01);
        ppu.write_2104(0xdd, &ctx);
        assert_eq!(ppu.oam[0x200], 0xdd);
        assert_eq!(ppu.oam_addr, 0x201);
        ppu.oam_addr = 0x250;
        ppu.write_2104(0xee, &ctx);
        assert_eq!(ppu.oam[0x210], 0xee);
        // The latch still takes an even byte even here.
        assert_eq!(ppu.oam_latchdata, 0xee);
    }

    #[test]
    fn reads_return_the_byte_and_move_on() {
        let mut ppu = Ppu::new();
        let ctx = blank_ctx();
        ppu.oam[0..3].copy_from_slice(&[0x11, 0x22, 0x33]);
        ppu.write_2102(0x00);
        ppu.write_2103(0x00);
        assert_eq!(ppu.read_2138(&ctx), 0x11);
        assert_eq!(ppu.ppu1_mdr, 0x11);
        assert_eq!(ppu.read_2138(&ctx), 0x22);
        assert_eq!(ppu.read_2138(&ctx), 0x33);
        assert_eq!(ppu.oam_addr, 3);
        assert_eq!(ppu.oam_read_byte(0x3ff), ppu.oam[0x21f]);
    }

    #[test]
    fn the_running_address_wraps_at_ten_bits() {
        let mut ppu = Ppu::new();
        let ctx = blank_ctx();
        ppu.oam_addr = 0x3ff;
        ppu.write_2104(0xab, &ctx);
        assert_eq!(ppu.oam_addr, 0x000);
    }

    #[test]
    fn while_drawing_the_access_goes_where_the_evaluator_is() {
        let mut ppu = Ppu::new();
        ppu.display_disable = false;
        let drawing = PpuAccessCtx {
            mdr: 0,
            h_dot: 0,
            v_scanline: 50,
        };
        ppu.oam_iaddr = 0x200;
        ppu.oam_addr = 0x40;
        ppu.write_2104(0xcc, &drawing);
        assert_eq!(ppu.oam_addr, 0x41, "the running address moves anyway");
        assert_eq!(
            ppu.oam[0x200], 0xcc,
            "the byte went to the evaluator's slot"
        );
        // The parity that decides the latch is the running address's.
        assert_eq!(ppu.oam_latchdata, 0xcc);
        ppu.oam[0x201] = 0x5a;
        ppu.oam_iaddr = 0x201;
        assert_eq!(ppu.read_2138(&drawing), 0x5a);
        assert_eq!(ppu.oam_addr, 0x42);
    }

    #[test]
    fn the_vblank_reset_happens_only_while_drawing() {
        let mut ppu = Ppu::new();
        ppu.write_2102(0x08);
        ppu.oam_addr = 0x100;
        ppu.oam_reset_at_vblank();
        assert_eq!(ppu.oam_addr, 0x100, "forced blank: no reset");
        ppu.display_disable = false;
        ppu.oam_reset_at_vblank();
        assert_eq!(ppu.oam_addr, 0x10);
    }
}
