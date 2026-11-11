//! WMDATA / WMADD: the 5A22-side WRAM portal.
//!
//! The SNES exposes WRAM through four registers in the B-bus
//! window (`$2180-$2183`) that the 5A22 implements itself, the
//! PPU and APU are not involved. From the programmer's perspective
//! `WMADDL`, `WMADDH`, and `WMADDB` set a 17-bit pointer into the
//! 128 KiB WRAM; `WMDATA` reads or writes the byte at that pointer
//! and post-increments it.
//!
//! Because the data side has to touch WRAM, the state is owned by
//! [`crate::bus::SnesBus`] rather than the [`crate::bus::b_bus::BBus`]
//! dispatcher (which only knows about chips on the *other* side of
//! the B-bus). The bus's A-bus handler intercepts `$2180-$2183`
//! before delegating to the B-bus.

/// Pointer + helpers backing the `$2180-$2183` registers.
///
/// `wm_addr` is the 17-bit linear offset into the 128 KiB WRAM. We
/// store it as a `u32` (with `0x1_FFFF` masking applied on every
/// access) so the address fields can be assembled byte-by-byte
/// without worrying about temporary overflow.
#[derive(Default)]
pub(crate) struct WmdataState {
    wm_addr: u32,
}

impl WmdataState {
    pub(crate) fn new() -> Self {
        Self { wm_addr: 0 }
    }

    /// Capture the 17-bit WRAM portal pointer into a snapshot.
    pub(crate) fn to_savestate(&self) -> crate::savestate::WmdataSaveState {
        crate::savestate::WmdataSaveState {
            wm_addr: self.wm_addr,
        }
    }

    /// Restore the portal pointer from a snapshot (re-masked to 17 bits).
    // Mirrors `to_savestate`: restores into `&mut self` rather than
    // constructing, so it intentionally takes a receiver.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_savestate(&mut self, s: &crate::savestate::WmdataSaveState) {
        self.wm_addr = s.wm_addr & 0x0001_FFFF;
    }

    /// Current pointer (masked to 17 bits).
    pub(crate) fn addr(&self) -> u32 {
        self.wm_addr & 0x0001_FFFF
    }

    /// Read the byte at `wm_addr` from WRAM, then post-increment
    /// `wm_addr`. Returns 0 if WRAM is somehow smaller than 128 KiB
    /// (defensive, the real bus always passes the full WRAM).
    pub(crate) fn read_data(&mut self, wram: &[u8]) -> u8 {
        let addr = self.addr() as usize;
        let value = wram.get(addr).copied().unwrap_or(0);
        self.wm_addr = (self.wm_addr + 1) & 0x0001_FFFF;
        value
    }

    /// Write `value` to WRAM at `wm_addr`, then post-increment
    /// `wm_addr`. Out-of-range writes are silently dropped.
    pub(crate) fn write_data(&mut self, wram: &mut [u8], value: u8) {
        let addr = self.addr() as usize;
        if let Some(slot) = wram.get_mut(addr) {
            *slot = value;
        }
        self.wm_addr = (self.wm_addr + 1) & 0x0001_FFFF;
    }

    /// Write the low byte of `wm_addr` (register `$2181`).
    pub(crate) fn write_addr_low(&mut self, value: u8) {
        self.wm_addr = (self.wm_addr & 0xFFFF_FF00) | u32::from(value);
        self.wm_addr &= 0x0001_FFFF;
    }

    /// Write the middle byte of `wm_addr` (register `$2182`).
    pub(crate) fn write_addr_high(&mut self, value: u8) {
        self.wm_addr = (self.wm_addr & 0xFFFF_00FF) | (u32::from(value) << 8);
        self.wm_addr &= 0x0001_FFFF;
    }

    /// Write the bank byte of `wm_addr` (register `$2183`). Only
    /// bit 0 is wired on the real hardware (WRAM is 128 KiB =
    /// 17 bits of address).
    pub(crate) fn write_addr_bank(&mut self, value: u8) {
        self.wm_addr = (self.wm_addr & 0x0000_FFFF) | (u32::from(value & 0x01) << 16);
        self.wm_addr &= 0x0001_FFFF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmdata_write_then_read_round_trip() {
        let mut wram = vec![0u8; 0x20000];
        let mut wmd = WmdataState::new();
        wmd.write_data(&mut wram, 0x42);
        assert_eq!(wram[0], 0x42);
        assert_eq!(wmd.addr(), 1);
    }

    #[test]
    fn wmdata_auto_increment_on_read() {
        let mut wram = vec![0u8; 0x20000];
        wram[0] = 0xCC;
        wram[1] = 0xDD;
        let mut wmd = WmdataState::new();
        assert_eq!(wmd.read_data(&wram), 0xCC);
        assert_eq!(wmd.addr(), 1);
        assert_eq!(wmd.read_data(&wram), 0xDD);
        assert_eq!(wmd.addr(), 2);
    }

    #[test]
    fn wmdata_wraps_at_2_0000() {
        let mut wram = vec![0u8; 0x20000];
        let mut wmd = WmdataState::new();
        wmd.write_addr_low(0xFF);
        wmd.write_addr_high(0xFF);
        wmd.write_addr_bank(0x01);
        assert_eq!(wmd.addr(), 0x0001_FFFF);

        wmd.write_data(&mut wram, 0x77);
        assert_eq!(wram[0x0001_FFFF], 0x77);
        // Wrap: addr goes from $1_FFFF + 1 = $2_0000 -> masked to 0.
        assert_eq!(wmd.addr(), 0);

        // Confirm the wrap by writing again and seeing $7E:0000 hit.
        wmd.write_data(&mut wram, 0x88);
        assert_eq!(wram[0], 0x88);
        assert_eq!(wmd.addr(), 1);
    }

    #[test]
    fn wmdata_addr_setters_assemble_pointer() {
        let mut wmd = WmdataState::new();
        wmd.write_addr_low(0x34);
        wmd.write_addr_high(0x12);
        wmd.write_addr_bank(0x01);
        assert_eq!(wmd.addr(), 0x0001_1234);

        // High bits of the bank byte are ignored (only bit 0 is wired).
        wmd.write_addr_bank(0xFE);
        assert_eq!(wmd.addr(), 0x0000_1234);

        // Re-writing low does not clobber high / bank.
        wmd.write_addr_low(0xAB);
        assert_eq!(wmd.addr(), 0x0000_12AB);

        // Re-writing high does not clobber low / bank.
        wmd.write_addr_high(0x56);
        assert_eq!(wmd.addr(), 0x0000_56AB);
    }
}
