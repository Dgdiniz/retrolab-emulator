//! S-DD1 cartridge coprocessor (memory-mapping controller + graphics
//! decompressor).
//!
//! The S-DD1 (Star Ocean, Street Fighter Alpha 2) is a ROM bank
//! controller with an on-the-fly graphics decompressor. This module
//! implements the [`Coprocessor`] surface for both halves: the four MMC
//! super-bank registers plus the `$C0-$FF` ROM window they steer, and the
//! bulk decompression path that intercepts armed DMA transfers reading
//! that window.
//!
//! ## Decompression (the whole transfer, on its first matched read)
//!
//! The chip reconstructs each DMA channel's fixed A-bus source address and
//! byte count by passively spying on the console's `$43xx` register writes
//! ([`Sdd1Chip::observe_write`]), it never sees `$420B`. When a read of
//! the `$C0-$FF` window matches the fixed source address of a channel armed
//! for decompression (`sdd1_enable & xfer_enable`), the first matched read
//! decompresses the whole transfer into a buffer (see [`decomp`]); each
//! read then serves the next buffered byte, and draining the buffer clears
//! the channel's transfer-enable bit. Unmatched reads return the raw MMC
//! ROM byte.
//!
//! The console's DMA engine is untouched: a fixed-address transfer re-reads
//! the same window address once per byte, so successive buffered bytes reach
//! the PPU with the per-byte cycle accounting intact. The chip reads its
//! compressed source straight from its own `Arc<[u8]>`, so decompression
//! never re-enters the bus and needs no recursion guard. The model, a
//! chip that cannot see `$420B` and so watches the channel registers, is
//! the one bsnes-plus and MAME use; it was checked against both.
//!
//! ## Two windows, one chip
//!
//! A single [`Sdd1Chip`] answers on two disjoint bus windows, both routed
//! here as `MapTarget::ChipMmio` dispatches (see the map in
//! [`super::presets::sdd1_preset`]):
//!
//! - **Registers `$4800-$480F`.** Carved out of the CpuMmio page by the
//!   bus and dispatched with `base = SDD1_REG_WINDOW_BASE`, so the chip
//!   sees offsets `SDD1_REG_WINDOW_BASE..+0x10`. Only `$4800` (enable),
//!   `$4801` (transfer-enable), and `$4804-$4807` (the four MMC
//!   super-bank latches) are live.
//! - **ROM window `$C0-$FF:0000-FFFF`.** Dispatched with `base = 0`, so
//!   the chip sees a linear `0..0x40_0000` offset. The MMC maps each
//!   1 MiB quadrant of this window to a super-bank selected by `$4804+q`.
//!
//! The two windows never collide because [`SDD1_REG_WINDOW_BASE`] sits
//! far above the largest MMC offset, so a single magnitude test splits
//! them.
//!
//! ## MMC
//!
//! Four latches select a 1 MiB super-bank for each of the four quadrants
//! of the `$C0-$FF` window (`$C0-$CF`, `$D0-$DF`, `$E0-$EF`, `$F0-$FF`):
//!
//! ```text
//! q         = (offset >> 20) & 3          quadrant 0..3
//! within    = offset & 0x000F_FFFF        byte inside the 1 MiB quadrant
//! rom_off   = mmc[q] + within             mmc[q] is already super-bank << 20
//! ```
//!
//! A hardware reset latches the identity mapping (`mmc = {0,1,2,3} << 20`),
//! so the first 4 MiB of ROM appear linearly until the game reprograms the
//! latches. MAME masks each latch to `data & 7` (8 super-banks / 8 MiB max);
//! we do the same.

use std::sync::Arc;

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};

use crate::savestate::{CoprocessorState, Sdd1State};

// The pure bitstream decompressor, driven by the matched-read path below.
mod decomp;

/// `chip_id` the bus dispatch carries for the S-DD1. Distinct from the
/// DSP-1's id 1 so a (hypothetical) cartridge hosting both can place each
/// at its own slot; matches the `chips[].id` used in the manifest schema
/// example.
pub const SDD1_CHIP_ID: u8 = 5;

/// Chip-address base the register window (`$4800-$480F`) is dispatched
/// with, so the chip can tell a register access from an MMC ROM-window
/// access by magnitude alone. Chosen well above the largest MMC window
/// offset (`0x3F_FFFF`), and above any padded ROM, so the two windows
/// can never alias. The register slot masks the bus offset to `0..0xF`,
/// so a register access arrives as `SDD1_REG_WINDOW_BASE + reg`.
pub const SDD1_REG_WINDOW_BASE: u32 = 0x0400_0000;

/// One DMA channel's transfer parameters, reconstructed from the console's
/// `$43xx` register writes via [`Sdd1Chip::observe_write`]. The S-DD1 never
/// sees `$420B`; it spies on the per-channel A-bus source address and byte
/// count so a matched read of the `$C0-$FF` window can locate and size the
/// decompression.
#[derive(Clone, Copy, Default)]
struct DmaShadow {
    /// 24-bit A-bus source address (`$43x2-4`).
    addr: u32,
    /// Transfer byte count (`$43x5-6`); `0` means a full 65536-byte transfer.
    size: u16,
}

/// The S-DD1 as a [`Coprocessor`].
///
/// Holds the mapping state, the shared cartridge ROM (`Arc<[u8]>`, handed
/// over by the [`ChipFactory`](retro_core::cartridge::ChipFactory)), the
/// four MMC super-bank latches, and the two enable bitmasks, plus the
/// decompression working set (the per-channel DMA shadow and the pending
/// output buffer). The ROM/SRAM windows and program banks live in the
/// [`ComposableCartridge`](super::ComposableCartridge) that hosts this chip;
/// the chip only ever serves the `$C0-$FF` MMC window and its own registers.
pub struct Sdd1Chip {
    /// Cartridge ROM image, shared with the bus (the MMC indexes into it and
    /// the decompressor reads its compressed source from it).
    rom: Arc<[u8]>,
    /// Four MMC super-bank latches, each already shifted to a byte offset
    /// (`super_bank << 20`). Indexed by `$C0-$FF` window quadrant.
    mmc: [u32; 4],
    /// `$4800`: per-DMA-channel bitmask of channels enabled for
    /// decompression.
    sdd1_enable: u8,
    /// `$4801`: per-DMA-channel bitmask of channels armed for the next
    /// transfer. A channel's bit clears when its decompressed transfer
    /// drains.
    xfer_enable: u8,
    /// Per-channel DMA parameters spied off the `$43xx` writes, used to match
    /// an armed transfer's fixed source address and size its decompression.
    dma_shadow: [DmaShadow; 8],
    /// Pending decompressed bytes for the in-flight transfer: filled in one
    /// shot on the first matched read, then drained one byte per subsequent
    /// read. Empty when no transfer is being served.
    buffer: Vec<u8>,
    /// Read cursor into `buffer`.
    buffer_pos: usize,
    /// Decompression runs since reset (diagnostic: a non-zero count proves
    /// the decompression path actually fired).
    decompressions: u64,
    /// Generic chip metadata for the [`Coprocessor::info`] surface.
    info: ExpansionChipInfo,
    /// CPU accesses (reads + writes, both windows) since reset. A non-zero
    /// count proves the running game actually reaches the chip through the
    /// bus.
    accesses: u64,
}

impl Sdd1Chip {
    /// Build an S-DD1 over the cartridge ROM. `master_clock_hz` is unused
    /// (the S-DD1 has no free-running clock of its own) but kept in the
    /// signature to match the [`ChipFactory`](retro_core::cartridge::ChipFactory)
    /// seam. The MMC starts in its identity mapping.
    pub fn new(rom: Arc<[u8]>, _master_clock_hz: u64) -> Self {
        Self {
            rom,
            mmc: identity_mmc(),
            sdd1_enable: 0,
            xfer_enable: 0,
            dma_shadow: [DmaShadow::default(); 8],
            buffer: Vec::new(),
            buffer_pos: 0,
            decompressions: 0,
            info: ExpansionChipInfo {
                name: "S-DD1".to_string(),
                chip_type: ExpansionChipType::SDD1,
                description: "S-DD1 memory-mapping controller + decompressor".to_string(),
                features: vec!["mmc".to_string(), "decompress".to_string()],
            },
            accesses: 0,
        }
    }

    /// CPU accesses (reads + writes) to the chip since reset. Non-zero =>
    /// the running game reaches the chip through the bus.
    pub fn chip_accesses(&self) -> u64 {
        self.accesses
    }

    /// Resolve an MMC ROM-window offset (`0..0x40_0000`, the linear
    /// `$C0-$FF` window) to a raw ROM byte through the super-bank latches.
    fn read_mmc(&self, offset: u32) -> u8 {
        if self.rom.is_empty() {
            return 0;
        }
        self.rom[self.mmc_offset(offset)]
    }

    /// Resolve an MMC ROM-window offset to a raw ROM index through the
    /// super-bank latches, the byte the decompressor reads its compressed
    /// source from. Wraps modulo the ROM length; returns 0 for an empty ROM.
    fn mmc_offset(&self, offset: u32) -> usize {
        if self.rom.is_empty() {
            return 0;
        }
        let quadrant = ((offset >> 20) & 0x3) as usize;
        let within = offset & 0x000F_FFFF;
        let rom_off = self.mmc[quadrant].wrapping_add(within) as usize;
        rom_off % self.rom.len()
    }

    /// Read a chip register (`reg` is the low nibble of `$480x`). Unknown
    /// registers read back 0.
    fn read_register(&self, reg: u8) -> u8 {
        match reg {
            0x0 => self.sdd1_enable,
            0x1 => self.xfer_enable,
            // The four MMC latches read back the raw super-bank number.
            0x4..=0x7 => (self.mmc[(reg - 0x4) as usize] >> 20) as u8,
            _ => 0,
        }
    }

    /// Write a chip register (`reg` is the low nibble of `$480x`). Unknown
    /// registers drop the write.
    fn write_register(&mut self, reg: u8, value: u8) {
        match reg {
            0x0 => self.sdd1_enable = value,
            0x1 => self.xfer_enable = value,
            // MAME clamps the super-bank to `data & 7` (8 MiB max ROM).
            0x4..=0x7 => self.mmc[(reg - 0x4) as usize] = (u32::from(value) & 0x7) << 20,
            _ => {}
        }
    }
}

/// Identity MMC mapping: quadrant `i` selects super-bank `i`, i.e. the
/// first 4 MiB of ROM appear linearly across `$C0-$FF`.
fn identity_mmc() -> [u32; 4] {
    [0, 1, 2, 3].map(|i: u32| i << 20)
}

impl Coprocessor for Sdd1Chip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
        self.accesses += 1;
        if offset >= SDD1_REG_WINDOW_BASE {
            return self.read_register((offset - SDD1_REG_WINDOW_BASE) as u8 & 0xF);
        }

        // MMC ROM window. A read whose A-bus address matches the fixed source
        // of a channel armed for decompression (`sdd1_enable & xfer_enable`)
        // is a byte of that transfer: the first matched read decompresses the
        // whole transfer into `buffer`, and each read serves the next byte.
        // Draining the buffer clears the channel's transfer-enable bit.
        // Everything else is a plain MMC ROM read.
        let addr24 = 0xC0_0000 + offset;
        let armed = self.sdd1_enable & self.xfer_enable;
        for ch in 0..8 {
            if armed & (1 << ch) == 0 || self.dma_shadow[ch].addr != addr24 {
                continue;
            }
            if self.buffer.is_empty() {
                let src = self.mmc_offset(offset);
                let rom = Arc::clone(&self.rom);
                self.buffer = decomp::decompress(&rom[src..], self.dma_shadow[ch].size as usize);
                self.buffer_pos = 0;
                self.decompressions += 1;
            }
            let byte = self.buffer[self.buffer_pos];
            self.buffer_pos += 1;
            if self.buffer_pos >= self.buffer.len() {
                self.xfer_enable &= !(1 << ch);
                self.buffer.clear();
                self.buffer_pos = 0;
            }
            return byte;
        }
        self.read_mmc(offset)
    }

    fn write(&mut self, offset: u32, value: u8, _master_cycle: u64) {
        self.accesses += 1;
        if offset >= SDD1_REG_WINDOW_BASE {
            self.write_register((offset - SDD1_REG_WINDOW_BASE) as u8 & 0xF, value);
        }
        // The MMC ROM window is read-only; writes there are dropped.
    }

    fn observe_write(&mut self, addr: u32, value: u8) {
        // Passive spy on the console's `$43xx` DMA registers (the bus hands
        // over the bank-agnostic 16-bit offset). Reconstruct each channel's
        // A-bus source address (`$43x2-4`) and byte count (`$43x5-6`) so a
        // matched read can locate and size its decompression. This is not a
        // chip MMIO access, so `accesses` is left untouched.
        let lo = addr & 0xFFFF;
        let ch = ((lo >> 4) & 0x7) as usize;
        let shadow = &mut self.dma_shadow[ch];
        match lo & 0xF {
            0x2 => shadow.addr = (shadow.addr & 0xFFFF_FF00) | u32::from(value),
            0x3 => shadow.addr = (shadow.addr & 0xFFFF_00FF) | (u32::from(value) << 8),
            0x4 => shadow.addr = (shadow.addr & 0xFF00_FFFF) | (u32::from(value) << 16),
            0x5 => shadow.size = (shadow.size & 0xFF00) | u16::from(value),
            0x6 => shadow.size = (shadow.size & 0x00FF) | (u16::from(value) << 8),
            _ => {}
        }
    }

    fn reset(&mut self, _master_clock_hz: u64) {
        self.mmc = identity_mmc();
        self.sdd1_enable = 0;
        self.xfer_enable = 0;
        self.dma_shadow = [DmaShadow::default(); 8];
        self.buffer.clear();
        self.buffer_pos = 0;
        self.decompressions = 0;
        self.accesses = 0;
    }

    /// Persist the mapping state: the MMC latches, the two enable masks,
    /// and the per-channel DMA shadow, bincode-encoded as the console's
    /// typed [`CoprocessorState::Sdd1`] (the bridge in
    /// [`ComposableCartridge`](super::ComposableCartridge) decodes it back).
    /// The blob is always non-empty, the MMC latches are meaningful state
    /// even at reset, so an S-DD1 cartridge always contributes one snapshot
    /// entry. Diagnostics (`accesses`/`decompressions`) and the volatile
    /// decompression buffer are deliberately left out: they never affect the
    /// byte-exact re-save of a restored state (the DMA is atomic, so there is
    /// never a mid-decompress snapshot to reconstruct).
    fn serialize(&self) -> Vec<u8> {
        let mut dma_addr = [0u32; 8];
        let mut dma_size = [0u16; 8];
        for (i, shadow) in self.dma_shadow.iter().enumerate() {
            dma_addr[i] = shadow.addr;
            dma_size[i] = shadow.size;
        }
        let state = CoprocessorState::Sdd1(Sdd1State {
            mmc: self.mmc,
            sdd1_enable: self.sdd1_enable,
            xfer_enable: self.xfer_enable,
            dma_addr,
            dma_size,
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        // A garbage/empty blob or a foreign variant decodes to something we
        // can't consume and is dropped, leaving the chip untouched.
        let Ok(CoprocessorState::Sdd1(state)) = bincode::deserialize::<CoprocessorState>(data)
        else {
            return;
        };
        self.mmc = state.mmc;
        self.sdd1_enable = state.sdd1_enable;
        self.xfer_enable = state.xfer_enable;
        for (shadow, (&addr, &size)) in self
            .dma_shadow
            .iter_mut()
            .zip(state.dma_addr.iter().zip(state.dma_size.iter()))
        {
            shadow.addr = addr;
            shadow.size = size;
        }
        // The decompression buffer is volatile (never persisted): drop any
        // in-flight transfer so the restored chip starts each DMA fresh.
        self.buffer.clear();
        self.buffer_pos = 0;
    }

    fn debug_state(&self) -> ExpansionChipState {
        let reg = |address: u32, name: &str, value: u32, description: &str| ChipRegister {
            address,
            name: name.to_string(),
            value,
            description: description.to_string(),
        };
        let mut registers = vec![
            reg(
                0x4800,
                "SDD1EN",
                u32::from(self.sdd1_enable),
                "Channel decompression enable",
            ),
            reg(
                0x4801,
                "XFEREN",
                u32::from(self.xfer_enable),
                "Channel transfer arm",
            ),
            reg(0x4804, "MMC0", self.mmc[0] >> 20, "Super-bank for $C0-$CF"),
            reg(0x4805, "MMC1", self.mmc[1] >> 20, "Super-bank for $D0-$DF"),
            reg(0x4806, "MMC2", self.mmc[2] >> 20, "Super-bank for $E0-$EF"),
            reg(0x4807, "MMC3", self.mmc[3] >> 20, "Super-bank for $F0-$FF"),
            reg(
                0,
                "ACCESSES",
                self.accesses as u32,
                "CPU accesses since reset (truncated)",
            ),
            reg(
                0,
                "DECOMPS",
                self.decompressions as u32,
                "Decompression runs since reset (truncated)",
            ),
        ];
        // One row per DMA channel whose shadow `observe_write` reconstructed
        // (skip the zeroed defaults). The A-bus source address goes in the
        // value column; the byte count rides in the description.
        for ch in 0..8usize {
            let shadow = self.dma_shadow[ch];
            if shadow.addr == 0 && shadow.size == 0 {
                continue;
            }
            registers.push(reg(
                0x4302 + ((ch as u32) << 4),
                &format!("DMA{ch}"),
                shadow.addr,
                &format!("Ch {ch} A-bus src (size ${:04x})", shadow.size),
            ));
        }
        ExpansionChipState {
            chip_type: ExpansionChipType::SDD1,
            registers,
            internal_ram: None,
            program_counter: None,
            cycle_count: 0,
        }
    }

    fn label(&self) -> &str {
        "S-DD1"
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_HZ: u64 = 21_477_270;

    /// A 4 MiB ROM whose every byte encodes its own offset (low byte), so
    /// a read-back uniquely identifies which ROM offset the MMC resolved.
    fn ramp_rom(len: usize) -> Arc<[u8]> {
        let bytes: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
        Arc::from(bytes.into_boxed_slice())
    }

    /// Read a register through the public [`Coprocessor::read`] path, i.e.
    /// as the bus would after applying the register slot's base + mask.
    fn read_reg(chip: &mut Sdd1Chip, reg: u8) -> u8 {
        chip.read(SDD1_REG_WINDOW_BASE + u32::from(reg), 0)
    }

    /// Write a register through the public [`Coprocessor::write`] path.
    fn write_reg(chip: &mut Sdd1Chip, reg: u8, value: u8) {
        chip.write(SDD1_REG_WINDOW_BASE + u32::from(reg), value, 0);
    }

    #[test]
    fn resets_to_identity_mmc() {
        let chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        assert_eq!(chip.mmc, [0, 1 << 20, 2 << 20, 3 << 20]);
        assert_eq!(chip.label(), "S-DD1");
        assert_eq!(chip.info().chip_type, ExpansionChipType::SDD1);
    }

    #[test]
    fn identity_mmc_reads_first_four_mib_linearly() {
        let rom = ramp_rom(0x40_0000); // 4 MiB
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        // Quadrant 0 ($C0-$CF window) -> super-bank 0, offset 0x1234.
        assert_eq!(chip.read(0x00_1234, 0), rom[0x00_1234]);
        // Quadrant 1 ($D0-$DF window, offset >> 20 == 1) -> super-bank 1.
        assert_eq!(chip.read(0x10_5678, 0), rom[0x10_5678]);
        // Quadrant 3 ($F0-$FF window) -> super-bank 3.
        assert_eq!(chip.read(0x30_ABCD, 0), rom[0x30_ABCD]);
    }

    #[test]
    fn mmc_latch_selects_super_bank() {
        let rom = ramp_rom(0x80_0000); // 8 MiB (super-banks 0..7)
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);

        // Point quadrant 0 ($C0-$CF) at super-bank 5.
        write_reg(&mut chip, 0x4, 5);
        // Reading window offset 0x0_2000 (quadrant 0) now resolves to
        // super-bank 5 base (5 MiB) + 0x2000.
        let want = rom[(5 << 20) + 0x2000];
        assert_eq!(chip.read(0x00_2000, 0), want);
        // Read-back returns the raw super-bank number.
        assert_eq!(read_reg(&mut chip, 0x4), 5);
    }

    #[test]
    fn mmc_latch_is_clamped_to_three_bits() {
        let rom = ramp_rom(0x80_0000);
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        // 0xF5 & 0x7 == 5, so the high bits are dropped (MAME parity).
        write_reg(&mut chip, 0x6, 0xF5);
        assert_eq!(read_reg(&mut chip, 0x6), 5);
        // Quadrant 2 ($E0-$EF window, offset >> 20 == 2) -> super-bank 5.
        assert_eq!(chip.read(0x20_0010, 0), rom[(5 << 20) + 0x10]);
    }

    #[test]
    fn mmc_wraps_when_offset_exceeds_rom() {
        // A 1 MiB ROM: super-bank 3 + within wraps back through modulo.
        let rom = ramp_rom(0x10_0000);
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        // Quadrant 3 (offset >> 20 == 3) with identity MMC -> super-bank 3,
        // i.e. byte offset 3 MiB, which wraps into the 1 MiB image.
        let got = chip.read(0x30_0007, 0);
        assert_eq!(got, rom[((3 << 20) + 0x7) % rom.len()]);
    }

    #[test]
    fn enable_registers_round_trip() {
        let mut chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        write_reg(&mut chip, 0x0, 0xA5); // $4800 sdd1_enable
        write_reg(&mut chip, 0x1, 0x3C); // $4801 xfer_enable
        assert_eq!(read_reg(&mut chip, 0x0), 0xA5);
        assert_eq!(read_reg(&mut chip, 0x1), 0x3C);
    }

    #[test]
    fn unknown_registers_read_zero_and_drop_writes() {
        let mut chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        write_reg(&mut chip, 0x2, 0xFF); // $4802: unknown
        write_reg(&mut chip, 0xF, 0xFF); // $480F: unknown
        assert_eq!(read_reg(&mut chip, 0x2), 0);
        assert_eq!(read_reg(&mut chip, 0xF), 0);
    }

    #[test]
    fn register_and_mmc_windows_do_not_collide() {
        // A read at MMC window offset 0 (from $C0:0000) must hit ROM, not
        // register 0, the magnitude split is what keeps them apart.
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        write_reg(&mut chip, 0x0, 0x7E); // set sdd1_enable
        assert_eq!(
            chip.read(0x00_0000, 0),
            rom[0],
            "offset 0 is the MMC window"
        );
        assert_eq!(
            read_reg(&mut chip, 0x0),
            0x7E,
            "register 0 is the enable latch"
        );
    }

    #[test]
    fn writes_to_mmc_window_are_dropped() {
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        chip.write(0x00_0000, 0xFF, 0); // MMC window write
        assert_eq!(chip.read(0x00_0000, 0), rom[0], "ROM window is read-only");
    }

    #[test]
    fn accesses_count_reads_and_writes_and_reset_clears() {
        let mut chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        assert_eq!(chip.chip_accesses(), 0);
        chip.read(0x00_0000, 0); // MMC read
        write_reg(&mut chip, 0x4, 1); // register write
        read_reg(&mut chip, 0x4); // register read
        assert_eq!(chip.chip_accesses(), 3);
        assert_eq!(chip.accesses(), 3);
        chip.reset(NTSC_HZ);
        assert_eq!(chip.chip_accesses(), 0);
        assert_eq!(
            chip.mmc,
            [0, 1 << 20, 2 << 20, 3 << 20],
            "reset restores identity MMC"
        );
    }

    #[test]
    fn debug_state_surfaces_mmc_and_enables() {
        let mut chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        write_reg(&mut chip, 0x5, 6); // mmc[1] = super-bank 6
        write_reg(&mut chip, 0x0, 0x81); // sdd1_enable
        let st = chip.debug_state();
        assert_eq!(st.chip_type, ExpansionChipType::SDD1);
        assert!(st.internal_ram.is_none());
        assert_eq!(st.program_counter, None);
        let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
        for want in ["SDD1EN", "XFEREN", "MMC0", "MMC1", "ACCESSES"] {
            assert!(names.contains(&want), "missing register {want}");
        }
        let mmc1 = st.registers.iter().find(|r| r.name == "MMC1").unwrap();
        assert_eq!(mmc1.value, 6);
    }

    #[test]
    fn savestate_round_trips_mapping_state() {
        let mut chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        // Drive every persisted field away from its reset default: all four
        // MMC latches, both enable masks, and two channels' DMA shadow.
        write_reg(&mut chip, 0x4, 1);
        write_reg(&mut chip, 0x5, 2);
        write_reg(&mut chip, 0x6, 3);
        write_reg(&mut chip, 0x7, 4);
        write_reg(&mut chip, 0x0, 0xA5); // sdd1_enable
        write_reg(&mut chip, 0x1, 0x3C); // xfer_enable
        set_shadow(&mut chip, 2, 0xC1_2345, 0x0100);
        set_shadow(&mut chip, 7, 0xC7_6543, 0x0000);

        let saved = chip.serialize();
        assert!(!saved.is_empty(), "S-DD1 always persists its mapping state");

        // Restore into a fresh chip (identity MMC, zero enables) and confirm
        // every persisted field came back.
        let mut restored = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        restored.restore(&saved, NTSC_HZ);
        assert_eq!(restored.mmc, [1 << 20, 2 << 20, 3 << 20, 4 << 20]);
        assert_eq!(read_reg(&mut restored, 0x0), 0xA5);
        assert_eq!(read_reg(&mut restored, 0x1), 0x3C);
        assert_eq!(restored.dma_shadow[2].addr, 0xC1_2345);
        assert_eq!(restored.dma_shadow[2].size, 0x0100);
        assert_eq!(restored.dma_shadow[7].addr, 0xC7_6543);
        assert_eq!(restored.dma_shadow[7].size, 0x0000);

        // Re-saving immediately after the restore reproduces the same bytes:
        // proof no persisted field was dropped on the way through.
        assert_eq!(restored.serialize(), saved, "resave must be byte-identical");
    }

    #[test]
    fn savestate_blob_is_non_empty_at_reset() {
        // Unlike the DSP-1 idle stub, the S-DD1's MMC latches are always
        // meaningful, so even a just-reset chip contributes a snapshot entry.
        let chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        assert!(!chip.serialize().is_empty());
    }

    #[test]
    fn restore_ignores_garbage_and_foreign_variants() {
        let mut chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        // Seed mmc[0] = super-bank 5 so we can prove a bad restore leaves it be.
        write_reg(&mut chip, 0x4, 5);
        // Garbage bytes decode to nothing -> dropped, state untouched.
        chip.restore(&[1, 2, 3], NTSC_HZ);
        assert_eq!(read_reg(&mut chip, 0x4), 5);
        // A foreign backend blob (DSP-1 HLE) is a different variant -> dropped.
        let foreign = bincode::serialize(&CoprocessorState::Dsp1Hle(
            crate::savestate::Dsp1HleState::default(),
        ))
        .unwrap();
        chip.restore(&foreign, NTSC_HZ);
        assert_eq!(
            read_reg(&mut chip, 0x4),
            5,
            "foreign variant left the S-DD1 state intact"
        );
    }

    #[test]
    fn restore_clears_inflight_decompression_buffer() {
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        let addr24 = 0xC0_0000 + 0x8000;
        let offset = addr24 - 0xC0_0000;
        arm_channel(&mut chip, 1, addr24, 32);
        // Partially drain so a buffer is in flight.
        let _ = chip.read(offset, 0);
        let _ = chip.read(offset, 0);
        assert!(!chip.buffer.is_empty());
        assert_eq!(chip.buffer_pos, 2);

        // Restoring a (default) state drops the volatile buffer entirely.
        let blob = Sdd1Chip::new(rom, NTSC_HZ).serialize();
        chip.restore(&blob, NTSC_HZ);
        assert!(chip.buffer.is_empty(), "restore drops the in-flight buffer");
        assert_eq!(chip.buffer_pos, 0);
    }

    #[test]
    fn debug_state_surfaces_dma_shadow_rows() {
        let mut chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        // No DMA rows before any $43xx write was observed.
        let names: Vec<String> = chip
            .debug_state()
            .registers
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert!(
            !names.iter().any(|n| n.starts_with("DMA")),
            "no DMA rows at reset, got {names:?}"
        );

        // After observing channel 5's transfer, exactly its row appears with
        // the source address in the value column and the size in the text.
        set_shadow(&mut chip, 5, 0xC3_4567, 0x1234);
        let st = chip.debug_state();
        let dma5 = st
            .registers
            .iter()
            .find(|r| r.name == "DMA5")
            .expect("channel 5 DMA row present");
        assert_eq!(dma5.value, 0xC3_4567);
        assert!(dma5.description.contains("$1234"), "size in description");
        assert!(
            !st.registers.iter().any(|r| r.name == "DMA0"),
            "untouched channels stay hidden"
        );
    }

    // -- Decompression path (observe_write + matched-read) ---------------

    /// Reconstruct one channel's `dma_shadow` by feeding the `$43x2-6`
    /// writes the bus would forward via `observe_write` (bank-agnostic 16-bit
    /// offset).
    fn set_shadow(chip: &mut Sdd1Chip, ch: u8, addr24: u32, size: u16) {
        let base = 0x4300 + (u32::from(ch) << 4);
        chip.observe_write(base + 0x2, (addr24 & 0xFF) as u8);
        chip.observe_write(base + 0x3, ((addr24 >> 8) & 0xFF) as u8);
        chip.observe_write(base + 0x4, ((addr24 >> 16) & 0xFF) as u8);
        chip.observe_write(base + 0x5, (size & 0xFF) as u8);
        chip.observe_write(base + 0x6, ((size >> 8) & 0xFF) as u8);
    }

    /// Arm channel `ch` for decompression: set its `$4800`/`$4801` bits and
    /// its DMA shadow. ORs the enable bits so multiple channels can be armed.
    fn arm_channel(chip: &mut Sdd1Chip, ch: u8, addr24: u32, size: u16) {
        let en = read_reg(chip, 0x0) | (1 << ch);
        let xf = read_reg(chip, 0x1) | (1 << ch);
        write_reg(chip, 0x0, en);
        write_reg(chip, 0x1, xf);
        set_shadow(chip, ch, addr24, size);
    }

    #[test]
    fn observe_write_reconstructs_shadow_per_channel() {
        let mut chip = Sdd1Chip::new(ramp_rom(0x40_0000), NTSC_HZ);
        // Channel 5 (base $4350): source $C3:4567, size $1234.
        set_shadow(&mut chip, 5, 0xC3_4567, 0x1234);
        assert_eq!(chip.dma_shadow[5].addr, 0xC3_4567);
        assert_eq!(chip.dma_shadow[5].size, 0x1234);
        // Other channels stay at their default (zeroed) shadow.
        assert_eq!(chip.dma_shadow[0].addr, 0);
        assert_eq!(chip.dma_shadow[0].size, 0);
        // Observation is passive, it must not count as a chip access.
        assert_eq!(chip.accesses(), 0);
    }

    #[test]
    fn model_c_serves_decompressed_bytes_and_clears_xfer_on_drain() {
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        let ch = 3u8;
        let addr24 = 0xC0_0000 + 0x1_2340;
        let size = 16u16;
        arm_channel(&mut chip, ch, addr24, size);

        let offset = addr24 - 0xC0_0000;
        let src = offset as usize % rom.len();
        let expected = super::decomp::decompress(&rom[src..], size as usize);
        assert_eq!(expected.len(), size as usize);
        // Sanity: the oracle is non-trivial, decompression transforms the
        // source bytes, so a matched read is not a raw-ROM passthrough.
        let raw: Vec<u8> = (0..size as usize)
            .map(|i| rom[(src + i) % rom.len()])
            .collect();
        assert_ne!(expected, raw, "decompression must transform the source");

        // Successive matched reads drain the decompressed buffer in order.
        let got: Vec<u8> = (0..size).map(|_| chip.read(offset, 0)).collect();
        assert_eq!(got, expected);
        assert_eq!(
            chip.decompressions, 1,
            "one bulk decompress for the transfer"
        );

        // Draining cleared this channel's transfer-enable bit (but not its
        // decompression-enable bit).
        assert_eq!(read_reg(&mut chip, 0x1) & (1 << ch), 0, "xfer bit cleared");
        assert_ne!(read_reg(&mut chip, 0x0) & (1 << ch), 0, "enable bit kept");

        // A further read at the same address is now unarmed -> raw MMC ROM.
        assert_eq!(chip.read(offset, 0), rom[src]);
        assert_eq!(chip.decompressions, 1, "no re-decompress once xfer cleared");
    }

    #[test]
    fn decompression_gated_on_enable_and_xfer_and_address_match() {
        let rom = ramp_rom(0x40_0000);
        let addr24 = 0xC0_0000 + 0x5000;
        let offset = addr24 - 0xC0_0000;
        let src = offset as usize % rom.len();

        // Decompression-enable set but not transfer-armed -> raw MMC.
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        write_reg(&mut chip, 0x0, 1 << 2);
        set_shadow(&mut chip, 2, addr24, 8);
        assert_eq!(chip.read(offset, 0), rom[src]);
        assert_eq!(chip.decompressions, 0);

        // Transfer-armed but decompression-disabled -> raw MMC.
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        write_reg(&mut chip, 0x1, 1 << 2);
        set_shadow(&mut chip, 2, addr24, 8);
        assert_eq!(chip.read(offset, 0), rom[src]);
        assert_eq!(chip.decompressions, 0);

        // Fully armed, but a read at a *different* address doesn't match the
        // channel's fixed source -> raw MMC at that other offset.
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        arm_channel(&mut chip, 2, addr24, 8);
        let other = offset + 0x10;
        assert_eq!(chip.read(other, 0), rom[other as usize % rom.len()]);
        assert_eq!(chip.decompressions, 0);
    }

    #[test]
    fn size_zero_decompresses_full_65536_transfer() {
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        let addr24 = 0xC0_0000 + 0x2_0000;
        let offset = addr24 - 0xC0_0000;
        let src = offset as usize % rom.len();
        arm_channel(&mut chip, 0, addr24, 0); // size 0 => full 65536-byte transfer

        let expected = super::decomp::decompress(&rom[src..], 0);
        assert_eq!(expected.len(), 0x1_0000);

        let got: Vec<u8> = (0..expected.len()).map(|_| chip.read(offset, 0)).collect();
        assert_eq!(got, expected);
        assert_eq!(
            chip.decompressions, 1,
            "one bulk decompress for the whole transfer"
        );
        assert_eq!(
            read_reg(&mut chip, 0x1) & 1,
            0,
            "xfer clears after the final byte"
        );
    }

    #[test]
    fn reset_clears_decompression_state() {
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sdd1Chip::new(rom.clone(), NTSC_HZ);
        let addr24 = 0xC0_0000 + 0x8000;
        let offset = addr24 - 0xC0_0000;
        arm_channel(&mut chip, 1, addr24, 32);
        // Partially drain so buffer/pos/decompressions are mid-transfer.
        let _ = chip.read(offset, 0);
        let _ = chip.read(offset, 0);
        assert_eq!(chip.decompressions, 1);
        assert!(!chip.buffer.is_empty());
        assert_eq!(chip.buffer_pos, 2);

        chip.reset(NTSC_HZ);
        assert_eq!(chip.decompressions, 0);
        assert!(chip.buffer.is_empty());
        assert_eq!(chip.buffer_pos, 0);
        assert_eq!(chip.dma_shadow[1].addr, 0);
        assert_eq!(chip.dma_shadow[1].size, 0);
        // Reset also restores the identity MMC and clears both enables.
        assert_eq!(chip.mmc, [0, 1 << 20, 2 << 20, 3 << 20]);
        assert_eq!(read_reg(&mut chip, 0x0), 0);
        assert_eq!(read_reg(&mut chip, 0x1), 0);
    }
}
