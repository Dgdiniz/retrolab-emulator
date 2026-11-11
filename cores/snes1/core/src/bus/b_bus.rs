//! Internal B-bus dispatcher.
//!
//! The SNES exposes the PPU and APU ports through a physically
//! separate 8-bit B-bus visible at `$00-$3F + $80-$BF : $2100-$21FF`.
//! On the real hardware the 5A22 latches the A-bus low byte into the
//! B-bus address whenever the upper bits match the window, but DMA
//! can drive the B-bus *directly* while the A-bus carries something
//! else, that parallel strobing is why we keep the B-bus as its own
//! object instead of letting the A-bus decoder talk to PPU/APU
//! handles directly.
//!
//! The dispatcher routes by `reg_offset` (the bottom byte of the
//! A-bus address minus `$2100`):
//!
//! | range          | subsystem                                |
//! |----------------|------------------------------------------|
//! | `$00-$3F`      | PPU registers (real [`Ppu`])             |
//! | `$40-$7F`      | APU mailbox, intercepted *upstream* by [`crate::bus::SnesBus`] against the real [`crate::apu::Apu`]; never reaches here |
//! | `$80-$83`      | WMDATA / WMADD, intercepted *upstream* by [`crate::bus::SnesBus`]; never reaches here |
//! | `$84-$FF`      | Expansion / reserved -> MDR                |
//!
//! Several PPU register handlers need timing-side inputs (the
//! current H dot, the current scanline, whether the renderer is
//! in active draw) that the bus has no business storing inside the
//! BBus itself. Those inputs travel through a [`PpuAccessCtx`]
//! built fresh on each call by the parent `SnesBus`, which caches
//! the raster snapshot via `SnesBus::set_raster_snapshot`.
//!
//! `$213C` / `$213D` / `$213F` are intercepted by the parent
//! `SnesBus` because the H/V counter latches and the
//! `counters_latched` flag live in [`crate::system_regs::SystemRegs`].
//! The BBus dispatcher only needs to know about PPU and APU
//! registers per the table above.

use crate::ppu::{Ppu, PpuAccessCtx};

/// The internal B-bus, owned exclusively by [`crate::bus::SnesBus`].
///
/// `reg_offset` parameters are zero-based inside the 256-byte B-bus
/// window: `reg_offset = (lo_addr - 0x2100) as u8`, where `lo_addr`
/// is the bottom 16 bits of the A-bus address.
///
/// The APU mailbox (`$2140-$2143` and its `$2144-$217F` mirrors) is
/// *not* dispatched here: `SnesBus` intercepts that range upstream and
/// routes it through the real [`crate::apu::Apu`], which must catch the
/// SPC700 up before the access. The dispatcher therefore only owns the
/// PPU.
pub(crate) struct BBus {
    ppu: Ppu,
}

impl BBus {
    /// Build a fresh B-bus dispatcher.
    pub(crate) fn new() -> Self {
        Self { ppu: Ppu::new() }
    }

    /// Borrow the underlying PPU. Useful for the debugger and for
    /// tests that want to inspect VRAM / OAM / CGRAM without
    /// going through the dispatcher.
    pub(crate) fn ppu(&self) -> &Ppu {
        &self.ppu
    }

    /// Mutably borrow the PPU. Used by `SnesBus::ppu_mut` for the
    /// same purpose as [`Self::ppu`].
    pub(crate) fn ppu_mut(&mut self) -> &mut Ppu {
        &mut self.ppu
    }

    /// Read driven by the A-bus.
    ///
    /// `ctx` carries `mdr` + `(h_dot, v_scanline)`. The PPU needs
    /// the raster snapshot to gate OAM/CGRAM active-scan accesses
    /// and to build STAT77; the BBus itself stays raster-free. DMA's
    /// B-bus reads come here too, through the parent bus's dispatch,
    /// with the same `ctx`.
    pub(crate) fn read(&mut self, reg_offset: u8, ctx: PpuAccessCtx) -> u8 {
        match reg_offset {
            0x00..=0x3F => self.ppu.read_reg(reg_offset, &ctx),
            // $40-$7F (APU mailbox) and $80-$83 (WMDATA / WMADD) are
            // both intercepted by SnesBus before delegating here, so
            // they should never arrive. $84-$FF is reserved /
            // expansion. Open bus (MDR) is the safe fallback for all.
            _ => ctx.mdr,
        }
    }

    /// Write driven by the A-bus.
    pub(crate) fn write(&mut self, reg_offset: u8, value: u8, ctx: PpuAccessCtx) {
        // See `read`: $40-$7F (APU) and $80-$83 (WMDATA) are intercepted
        // by SnesBus before delegating here, and $84-$FF is reserved /
        // expansion. Every non-PPU write drops.
        if let 0x00..=0x3F = reg_offset {
            self.ppu.write_reg(reg_offset, value, &ctx);
        }
    }

    /// Read for DMA that does **not** touch the MDR latch on the
    /// A-bus side, which might be carrying an unrelated fetch in the
    /// same master cycle. A stub that returns 0: the DMA engine does
    /// not call it, its B-bus reads come through [`Self::read`].
    #[allow(dead_code)]
    pub(crate) fn dma_read(&mut self, _reg_offset: u8) -> u8 {
        0
    }

    /// Write for DMA that does **not** touch the MDR latch on the
    /// A-bus side. A stub that drops the write: the DMA engine does
    /// not call it, its B-bus writes go through [`Self::write`].
    #[allow(dead_code)]
    pub(crate) fn dma_write(&mut self, _reg_offset: u8, _value: u8) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(mdr: u8) -> PpuAccessCtx {
        PpuAccessCtx {
            mdr,
            h_dot: 0,
            v_scanline: 0,
        }
    }

    #[test]
    fn dispatch_ppu_write_only_registers_read_as_mdr() {
        let mut bus = BBus::new();
        // $2100 INIDISP is write-only; reading should return the
        // A-bus MDR via the PPU's catch-all branch.
        assert_eq!(bus.read(0x00, ctx(0x42)), 0x42);
    }

    #[test]
    fn dispatch_ppu_round_trip_via_real_register() {
        let mut bus = BBus::new();
        // CGADD + CGDATA gives us a directly observable write
        // path that doesn't depend on the bus's raster snapshot.
        bus.write(0x21, 0x00, ctx(0)); // CGADD: colour 0
        bus.write(0x22, 0x12, ctx(0)); // CGDATA low byte (latched)
        bus.write(0x22, 0x34, ctx(0)); // CGDATA high byte (commit pair, 0x7F mask)
        let cgram_word = bus.ppu().cgram()[0];
        assert_eq!(cgram_word, 0x3412 & 0x7FFF);
    }

    #[test]
    fn dispatch_apu_range_falls_through() {
        // The APU mailbox ($40-$7F) is intercepted by SnesBus and
        // routed through the real Apu, so the dispatcher must never
        // service it. If a future refactor lets it through, the safe
        // fallback is open bus (MDR), this pins that contract, and
        // writes must drop without panicking. The real mailbox
        // behaviour is covered end-to-end in tests/apu_test.rs.
        let mut bus = BBus::new();
        for reg in (0x40u8..=0x7F).step_by(4) {
            assert_eq!(bus.read(reg, ctx(0xAA)), 0xAA);
        }
        for reg in 0x40u8..=0x7F {
            bus.write(reg, 0xCC, ctx(0)); // smoke: must not panic
        }
    }

    #[test]
    fn dispatch_expansion_returns_mdr() {
        let mut bus = BBus::new();
        for reg in 0x84u8..=0xFF {
            assert_eq!(bus.read(reg, ctx(0x5A)), 0x5A);
        }
        // Writes silently drop.
        for reg in 0x84u8..=0xFF {
            bus.write(reg, 0xCC, ctx(0));
        }
    }

    #[test]
    fn dispatch_wmdata_range_falls_through() {
        let mut bus = BBus::new();
        // The dispatcher is *expected* to never see $80-$83 because
        // the SnesBus intercepts those upstream. If a future
        // refactor accidentally lets WMDATA reach the BBus, the
        // safe fallback is open bus (MDR), this test pins that
        // contract so a silent regression is caught early.
        for reg in 0x80u8..=0x83 {
            assert_eq!(bus.read(reg, ctx(0xC3)), 0xC3);
        }
    }

    #[test]
    fn ppu_accessor_round_trips() {
        let mut bus = BBus::new();
        bus.write(0x00, 0x0F, ctx(0)); // INIDISP brightness = 15, no blank
        assert!(!bus.ppu().display_disable());
        assert_eq!(bus.ppu().display_brightness(), 0x0F);

        // Mutable access through ppu_mut.
        bus.ppu_mut().reset();
        assert!(bus.ppu().display_disable());
    }

    #[test]
    fn dma_read_does_not_touch_mdr() {
        // The DMA path is a stub (the DMA engine moves its bytes through
        // the bus's own dispatch instead): it returns 0 and does NOT take
        // or return MDR. This test documents that boundary.
        let mut bus = BBus::new();
        assert_eq!(bus.dma_read(0x00), 0);
        bus.dma_write(0x00, 0xCC); // smoke
    }
}
