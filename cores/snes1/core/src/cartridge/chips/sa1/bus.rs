//! The SA-1 CPU's bus adapter (`Sa1Bus`), the SA-1's own view of the address
//! space, with the SA-1 wait-state model.
//!
//! The second 65816 does not see the S-CPU's bus. It sees its **own** map:
//!
//! | SA-1 address                        | Target                              |
//! |-------------------------------------|-------------------------------------|
//! | `$00-3F/$80-BF : 0000-07FF`         | I-RAM (low mirror)                  |
//! | `$00-3F/$80-BF : 2200-23FF`         | MMIO register file                 |
//! | `$00-3F/$80-BF : 3000-37FF`         | I-RAM                              |
//! | `$00-3F/$80-BF : 6000-7FFF`         | BW-RAM 8 KiB window (`$2225` cbm)   |
//! | `$00-3F/$80-BF : 8000-FFFF`         | ROM (Super MMC blocks C-F windows)  |
//! | `$40-4F        : 0000-FFFF`         | BW-RAM (linear)                    |
//! | `$60-6F        : 0000-FFFF`         | BW-RAM bitmap view (2bpp/4bpp)      |
//! | `$C0-FF        : 0000-FFFF`         | ROM (Super MMC blocks C-F, linear)  |
//! | anything else                       | open bus (the CPU's MDR latch)      |
//!
//! ROM goes through [`Sa1Chip::rom_via_mmc`], the Super MMC mapping the
//! S-CPU's view uses too, so both CPUs see the same blocks.
//!
//! ## Wait states
//!
//! The SA-1 counts time in ticks of 2 master cycles: an access takes one
//! tick, and a second when it reaches BW-RAM (the `$6000-7FFF` window, the
//! `$40-4F` banks or the `$60-6F` bitmap, [`is_bwram_addr`]); a cycle with
//! no access takes one. This adapter counts the ticks of the accesses in
//! [`Sa1Bus::ticks`] and the accesses in [`Sa1Bus::accesses`]; the burst
//! ([`super::Sa1Chip`]'s catch-up) takes the cycles with no access as the
//! CPU's count less the accesses, and turns it all into master cycles
//! (`ticks * 2 + internal * 2`). Checked against bsnes-plus and ares.

use retro_core::cpu::Addressable;

use super::bitmap::{bitmap_read, bitmap_write};
use super::Sa1Chip;

/// The SA-1 CPU's bus adapter. Borrows the chip so the SA-1's `Wdc65816`
/// (`mem::take`n out during a burst) can step against the rest of it; tallies
/// the SA-1 wait-state `tick()`s so the burst driver can advance the anchor.
pub(super) struct Sa1Bus<'a> {
    /// The chip minus its CPU (which the caller has taken out for the burst).
    pub(super) chip: &'a mut Sa1Chip,
    /// The ticks of this step's accesses: one each, two for BW-RAM. A tick is
    /// 2 master cycles.
    pub(super) ticks: u64,
    /// Memory accesses (reads + writes) this step. Subtracted from the CPU's
    /// reported cycle count to recover the internal (non-bus) cycles.
    pub(super) accesses: u64,
}

impl<'a> Sa1Bus<'a> {
    /// Wrap the chip for one burst step, with the tick/access tally cleared.
    pub(super) fn new(chip: &'a mut Sa1Chip) -> Self {
        Self {
            chip,
            ticks: 0,
            accesses: 0,
        }
    }

    /// Charge one access: one tick, and a second for a BW-RAM address.
    fn add_wait(&mut self, addr: u32) {
        self.accesses += 1;
        self.ticks += 1;
        if is_bwram_addr(addr) {
            self.ticks += 1;
        }
    }

    /// Decode the SA-1 view and read a byte (no wait-state tally, that is
    /// [`Self::add_wait`]'s job, called by the trait method).
    ///
    /// Takes `&mut self` because a read of `$2302` copies the timer's
    /// counters ([`Sa1Chip::mmio_read`]). While the burst has set
    /// [`Sa1Chip::vector_redirect`] for the instruction that takes an
    /// interrupt, a vector fetch at the top of bank `$00` reads CNV or CIV
    /// instead of ROM; a data read of the same address, with the flag clear,
    /// still reads ROM.
    fn decode_read(&mut self, addr: u32) -> u8 {
        if self.chip.vector_redirect {
            if let Some(byte) = self.chip.interrupts.sa1_vector_byte(addr) {
                return byte;
            }
        }
        let bank = (addr >> 16) as u8;
        let off = (addr & 0xFFFF) as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match off {
                // I-RAM is masked to 2 KiB inside `iram_read`, so `$0000-07FF`
                // and `$3000-37FF` both fold onto the same buffer.
                0x0000..=0x07FF | 0x3000..=0x37FF => self.chip.iram_read(off as u32),
                0x2200..=0x23FF => self.chip.mmio_read(off),
                0x6000..=0x7FFF => self.chip.bwram_window_sa1_read((off - 0x6000) as u32),
                0x8000..=0xFFFF => self.chip.rom_via_mmc(addr),
                // $0800-$21FF, $2400-$2FFF, $3800-$5FFF: no SA-1 mapping.
                _ => self.chip.cpu_mdr,
            },
            0x40..=0x4F => self.chip.bwram_linear_read(addr - 0x40_0000),
            // The 2bpp/4bpp bitmap view of BW-RAM (always bitmap here): the full
            // offset within $60-6F is the pixel address, decoded by `bbf`.
            0x60..=0x6F => bitmap_read(
                &self.chip.bwram,
                addr - 0x60_0000,
                self.chip.dma.bitmap_2bpp(),
            ),
            0xC0..=0xFF => self.chip.rom_via_mmc(addr),
            // $50-$5F, $70-$7F: no SA-1 mapping.
            _ => self.chip.cpu_mdr,
        }
    }

    /// Decode the SA-1 view and write a byte (ROM/unmapped writes are dropped).
    fn decode_write(&mut self, addr: u32, value: u8) {
        let bank = (addr >> 16) as u8;
        let off = (addr & 0xFFFF) as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match off {
                0x0000..=0x07FF | 0x3000..=0x37FF => self.chip.iram_write(off as u32, value),
                0x2200..=0x23FF => self.chip.mmio_write(off, value),
                0x6000..=0x7FFF => self
                    .chip
                    .bwram_window_sa1_write((off - 0x6000) as u32, value),
                // ROM is read-only; other offsets are unmapped.
                _ => {}
            },
            0x40..=0x4F => self.chip.bwram_linear_write(addr - 0x40_0000, value),
            // Bitmap-view write: read-modify-write the packed BW-RAM byte,
            // tracking the dirty flag only on a real change (like `store_bwram`).
            0x60..=0x6F
                if bitmap_write(
                    &mut self.chip.bwram,
                    addr - 0x60_0000,
                    value,
                    self.chip.dma.bitmap_2bpp(),
                ) =>
            {
                self.chip.bwram_dirty = true;
            }
            // ROM ($C0-FF) is read-only; $50-5F/$70-7F are unmapped.
            _ => {}
        }
    }
}

impl Addressable for Sa1Bus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        let addr = addr & 0xFF_FFFF;
        self.add_wait(addr);
        let value = self.decode_read(addr);
        self.chip.cpu_mdr = value;
        value
    }

    fn write(&mut self, addr: u32, value: u8) {
        let addr = addr & 0xFF_FFFF;
        self.add_wait(addr);
        self.chip.cpu_mdr = value;
        self.decode_write(addr, value);
    }
}

/// The bank of a 24-bit address, and whether it is a system bank
/// (`$00-$3F` or `$80-$BF`).
fn bank(addr: u32) -> (u8, bool) {
    let bank = (addr >> 16) as u8;
    (bank, bank & 0x40 == 0)
}

/// The SA-1's BW-RAM window: `$6000-$7FFF` of a system bank.
pub(super) fn in_bwram_window(addr: u32) -> bool {
    let offset = addr as u16;
    bank(addr).1 && (0x6000..0x8000).contains(&offset)
}

/// All of BW-RAM in order: banks `$40-$4F`.
pub(super) fn in_bwram_banks(addr: u32) -> bool {
    (0x40..=0x4F).contains(&bank(addr).0)
}

/// ROM where a DMA copy can read it: `$8000-$FFFF` of a system bank, or any
/// of `$C0-$FF`.
pub(super) fn copy_reads_rom(addr: u32) -> bool {
    let (bank, system) = bank(addr);
    (system && addr as u16 >= 0x8000) || bank >= 0xC0
}

/// Whether an SA-1 address reaches BW-RAM, and so costs the second tick: the
/// window, the `$40-4F` banks, or the `$60-6F` bitmap.
fn is_bwram_addr(addr: u32) -> bool {
    in_bwram_window(addr) || in_bwram_banks(addr) || (0x60..=0x6F).contains(&bank(addr).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const NTSC_HZ: u64 = 21_477_270;

    fn ramp_rom(len: usize) -> Arc<[u8]> {
        let bytes: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
        Arc::from(bytes.into_boxed_slice())
    }

    #[test]
    fn bwram_addresses_are_the_window_the_linear_banks_and_the_bitmap() {
        // $00-3F/$80-BF : 6000-7FFF window.
        assert!(is_bwram_addr(0x00_6000));
        assert!(is_bwram_addr(0x3F_7FFF));
        assert!(is_bwram_addr(0x80_6000));
        assert!(is_bwram_addr(0xBF_7FFF));
        // $40-4F linear.
        assert!(is_bwram_addr(0x40_0000));
        assert!(is_bwram_addr(0x4F_FFFF));
        // $60-6F bitmap view.
        assert!(is_bwram_addr(0x60_0000));
        assert!(is_bwram_addr(0x6F_FFFF));
        // Not BW-RAM: I-RAM, ROM window, ROM linear, $50-5F, $70-7F.
        assert!(!is_bwram_addr(0x00_0010));
        assert!(!is_bwram_addr(0x00_8000));
        assert!(!is_bwram_addr(0xC0_0000));
        assert!(!is_bwram_addr(0x50_0000));
        assert!(!is_bwram_addr(0x70_0000));
    }

    #[test]
    fn iram_visible_low_and_at_3000() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        {
            let mut bus = Sa1Bus::new(&mut chip);
            bus.write(0x00_0010, 0xAB); // low I-RAM mirror
            assert_eq!(bus.read(0x00_0010), 0xAB);
            // The $3000 window aliases the same 2 KiB (offset 0x10).
            assert_eq!(bus.read(0x00_3010), 0xAB);
        }
        // The $80-BF banks alias I-RAM too.
        let mut bus = Sa1Bus::new(&mut chip);
        assert_eq!(bus.read(0x80_3010), 0xAB);
    }

    #[test]
    fn rom_window_block_c_and_linear_cx() {
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
        let mut bus = Sa1Bus::new(&mut chip);
        // $00:8000 -> block C LoROM-fixed base 0.
        assert_eq!(bus.read(0x00_8000), rom[0]);
        // $01:8000 -> +0x8000 (32 KiB/bank).
        assert_eq!(bus.read(0x01_8000), rom[0x8000]);
        // $C0:0000 -> block C selected, linear 64 KiB/bank.
        assert_eq!(bus.read(0xC0_0000), rom[0]);
        // $D0:0000 -> block D (bank 1) selected -> 0x100000.
        assert_eq!(bus.read(0xD0_0000), rom[0x10_0000]);
    }

    #[test]
    fn rom_writes_are_dropped() {
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
        let mut bus = Sa1Bus::new(&mut chip);
        bus.write(0x00_8000, 0xFF);
        bus.write(0xC0_0000, 0xFF);
        assert_eq!(bus.read(0x00_8000), rom[0], "ROM window unchanged");
        assert_eq!(bus.read(0xC0_0000), rom[0], "ROM linear unchanged");
    }

    #[test]
    fn bwram_window_uses_cbm_and_linear_spans_buffer() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x1_0000, true, NTSC_HZ); // 64 KiB
                                                                                   // Point the SA-1 window ($6000) at block 2 (cbm=2 -> offset 0x4000).
        chip.mmio_write(0x2225, 0x02);
        let mut bus = Sa1Bus::new(&mut chip);
        bus.write(0x00_6000, 0x5A);
        assert_eq!(bus.read(0x00_6000), 0x5A);
        // The linear view confirms it landed at cbm*0x2000 = 0x4000.
        assert_eq!(bus.read(0x40_4000), 0x5A);
        // A far linear write round-trips too.
        bus.write(0x40_ABCD, 0x3C);
        assert_eq!(bus.read(0x40_ABCD), 0x3C);
    }

    #[test]
    fn unmapped_reads_return_open_bus_mdr() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        let mut bus = Sa1Bus::new(&mut chip);
        // Seed the MDR via a real ROM read, then hit an unmapped hole.
        let seed = bus.read(0x00_8000);
        assert_eq!(bus.read(0x00_0900), seed, "open bus returns the last MDR");
        assert_eq!(bus.read(0x50_0000), seed);
        assert_eq!(bus.read(0x70_0000), seed);
    }

    #[test]
    fn tick_tally_charges_two_for_bwram_one_otherwise() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        // I-RAM read: one tick.
        let mut bus = Sa1Bus::new(&mut chip);
        bus.read(0x00_0010);
        assert_eq!((bus.ticks, bus.accesses), (1, 1));
        // ROM read: one tick.
        let mut bus = Sa1Bus::new(&mut chip);
        bus.read(0x00_8000);
        assert_eq!((bus.ticks, bus.accesses), (1, 1));
        // BW-RAM window read: two ticks, one access.
        let mut bus = Sa1Bus::new(&mut chip);
        bus.read(0x00_6000);
        assert_eq!((bus.ticks, bus.accesses), (2, 1));
        // BW-RAM linear write: two ticks, one access.
        let mut bus = Sa1Bus::new(&mut chip);
        bus.write(0x40_0000, 0x11);
        assert_eq!((bus.ticks, bus.accesses), (2, 1));
        // Several accesses accumulate.
        let mut bus = Sa1Bus::new(&mut chip);
        bus.read(0x00_0000); // I-RAM  +1
        bus.read(0x40_0000); // BW-RAM +2
        bus.write(0x00_0001, 0); // I-RAM +1
        assert_eq!((bus.ticks, bus.accesses), (4, 3));
    }

    #[test]
    fn bitmap_60_6f_view_4bpp_packs_and_reads_back() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        // BBF clear (4bpp): two pixels a byte.
        let mut bus = Sa1Bus::new(&mut chip);
        bus.write(0x60_0000, 0x0A); // pixel 0 -> low nibble
        bus.write(0x60_0001, 0x0B); // pixel 1 -> high nibble
        assert_eq!(bus.read(0x60_0000), 0x0A);
        assert_eq!(bus.read(0x60_0001), 0x0B);
        // The physical BW-RAM byte packs both nibbles (0xBA); the raw $40-4F
        // linear view (what the S-CPU also sees) confirms it.
        assert_eq!(bus.read(0x40_0000), 0xBA, "packed 4bpp byte");
    }

    #[test]
    fn bitmap_sw46_window_2bpp_packs_and_reads_back() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        chip.mmio_write(0x223F, 0x80); // bbf = 2bpp (4 pixels/byte)
        chip.mmio_write(0x2225, 0x80); // sw46 = 1 (bitmap window), cbm = 0
        let mut bus = Sa1Bus::new(&mut chip);
        // The $6000-7FFF window now decodes as a 2bpp bitmap at cbm block 0.
        bus.write(0x00_6000, 0b01); // pixel 0
        bus.write(0x00_6001, 0b10); // pixel 1
        bus.write(0x00_6002, 0b11); // pixel 2
        bus.write(0x00_6003, 0b01); // pixel 3
        assert_eq!(bus.read(0x00_6000), 0b01);
        assert_eq!(bus.read(0x00_6003), 0b01);
        // pixel3..0 packed high->low into physical byte 0.
        assert_eq!(bus.read(0x40_0000), 0b01_11_10_01, "packed 2bpp byte");
    }

    #[test]
    fn mmio_reaches_the_register_file() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        let mut bus = Sa1Bus::new(&mut chip);
        // Drive the arithmetic unit through the SA-1 bus: 6 * 7 = 42.
        bus.write(0x00_2250, 0x00); // MCNT = multiply
        bus.write(0x00_2251, 6);
        bus.write(0x00_2252, 0);
        bus.write(0x00_2253, 7);
        bus.write(0x00_2254, 0); // trigger
        assert_eq!(bus.read(0x00_2306), 42, "MR low byte via the SA-1 bus MMIO");
    }
}
