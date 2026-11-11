//! The GSU's view of the cartridge bus (`GsuBus`) plus a side-effect-free
//! peek variant (`GsuPeekBus`) for the disassembler.
//!
//! The GSU addresses a flat 8 MiB space (checked against bsnes-plus's
//! `superfxbus`, `bus/bus.cpp`): Game Pak ROM in the low banks (program
//! fetches, cache fills, and the ROM buffer read at `(ROMBR << 16) + R14`),
//! and Game Pak RAM at `0x70_0000 + (RAMBR << 16) + addr` (the RAM buffer and
//! the plot engine). The [`Gsu`](superfx_gsu::Gsu) computes those full
//! addresses and hands them here; this adapter folds each into the chip's
//! separate `rom`/`ram` buffers.
//!
//! **ROM is not linear.** The GSU's ROM bus shows two views of the *same*
//! image, as bsnes-plus maps it (`MapMode::Linear`): banks `$00-3F` are LoROM
//! (32 KiB/bank, the `$0000-7FFF` and `$8000-FFFF` halves alias the same
//! chunk) and banks `$40-5F` are the linear/HiROM view, both starting at ROM
//! offset 0. This is the exact counterpart of the S-CPU-facing windows in
//! `presets/superfx.rs`, so the GSU and the S-CPU recover identical bytes for
//! the same ROM location (a flat `rom[(PBR << 16) + addr]` would feed the GSU
//! S-CPU code for any bank `>= $01` and leave Star Fox, Yoshi's Island and
//! Doom on a black screen). See [`gsu_rom_offset`]. Game Pak RAM opens at
//! `0x70_0000`; an address wraps into the 8 MiB space (`& 0x7F_FFFF`).
//!
//! RON/RAN arbitration is resolved on the *S-CPU* side ([`super::SuperFxChip`]
//! `read`/`write`), so the GSU bus reads/writes ROM/RAM unconditionally, the
//! GSU always owns its own bus.

use retro_core::cpu::Addressable;

/// Game Pak RAM window base on the GSU's flat bus: `$70-71` (via `RAMBR`)
/// reach RAM, banks below reach ROM.
const GSU_RAM_BASE: u32 = 0x70_0000;

/// GSU bus address mask (8 MiB), the flat space the executor addresses (`$00-7F`).
const GSU_ADDR_MASK: u32 = 0x7F_FFFF;

/// First linear/HiROM bank on the GSU ROM bus (`$40`); banks below are LoROM.
const GSU_ROM_LINEAR_BANK: u32 = 0x40;

/// Fold a GSU-bus address (already masked to 8 MiB, below [`GSU_RAM_BASE`]) to a
/// linear Game Pak ROM offset, matching the Super FX `superfxbus` map
/// (checked against bsnes-plus `bus/bus.cpp`): banks `$00-3F` are LoROM (`bank * 0x8000 +` the 15-bit
/// in-bank offset, so both bank halves alias) and banks `$40-5F` are the linear
/// view of the same image (`addr - 0x40_0000`, starting at ROM offset 0). The
/// caller folds the result modulo the real ROM length.
#[inline]
fn gsu_rom_offset(addr: u32) -> u32 {
    let bank = addr >> 16;
    if bank < GSU_ROM_LINEAR_BANK {
        // LoROM: 32 KiB/bank; the low and high halves alias the same chunk.
        (bank << 15) | (addr & 0x7FFF)
    } else {
        // Linear (HiROM) view of the same image, continuing from offset 0.
        addr - (GSU_ROM_LINEAR_BANK << 16)
    }
}

/// The GSU CPU's live bus adapter. Borrows the chip's ROM (read-only) and
/// Game Pak RAM (mutable, with the battery dirty flag) so the [`Gsu`], taken
/// out of the chip for the burst, can `step` against them.
///
/// [`Gsu`]: superfx_gsu::Gsu
pub(super) struct GsuBus<'a> {
    /// Cartridge ROM image (read-only; folded modulo its length).
    pub(super) rom: &'a [u8],
    /// Game Pak RAM (folded modulo its length).
    pub(super) ram: &'a mut [u8],
    /// Set true when a RAM write lands, so the battery flush notices.
    pub(super) ram_dirty: &'a mut bool,
}

impl Addressable for GsuBus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        let a = addr & GSU_ADDR_MASK;
        if a >= GSU_RAM_BASE {
            read_folded(self.ram, a - GSU_RAM_BASE)
        } else {
            read_folded(self.rom, gsu_rom_offset(a))
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        let a = addr & GSU_ADDR_MASK;
        // ROM is read-only; only Game Pak RAM writes land (stores + plot).
        if a >= GSU_RAM_BASE && write_folded(self.ram, a - GSU_RAM_BASE, value) {
            *self.ram_dirty = true;
        }
    }
}

/// The GSU's peek bus for the disassembler: identical decode, but writes are
/// dropped and nothing is marked dirty, so `disassemble_secondary` never
/// perturbs Game Pak RAM (mirrors the SA-1's `Sa1PeekBus`).
pub(super) struct GsuPeekBus<'a> {
    /// Cartridge ROM image (read-only).
    pub(super) rom: &'a [u8],
    /// Game Pak RAM (read-only view).
    pub(super) ram: &'a [u8],
}

impl Addressable for GsuPeekBus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        let a = addr & GSU_ADDR_MASK;
        if a >= GSU_RAM_BASE {
            read_folded(self.ram, a - GSU_RAM_BASE)
        } else {
            read_folded(self.rom, gsu_rom_offset(a))
        }
    }

    fn write(&mut self, _addr: u32, _value: u8) {}
}

/// Read a byte from `buf`, folded modulo its length; an empty buffer reads as
/// open-bus `0`.
fn read_folded(buf: &[u8], off: u32) -> u8 {
    if buf.is_empty() {
        0
    } else {
        buf[(off as usize) % buf.len()]
    }
}

/// Write a byte to `buf`, folded modulo its length. Returns whether the write
/// landed (an empty buffer drops it), so the caller only marks the battery
/// dirty on a real store.
fn write_folded(buf: &mut [u8], off: u32, value: u8) -> bool {
    if buf.is_empty() {
        return false;
    }
    let len = buf.len();
    buf[(off as usize) % len] = value;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gsu_bus_splits_rom_low_and_ram_at_0x700000() {
        let rom = [0x11u8, 0x22, 0x33, 0x44];
        let mut ram = vec![0u8; 0x8000];
        let mut dirty = false;
        let mut bus = GsuBus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        // Low addresses read ROM (folded modulo its length).
        assert_eq!(bus.read(0), 0x11);
        assert_eq!(bus.read(1), 0x22);
        assert_eq!(bus.read(4), 0x11, "ROM folds modulo its length");
        // $70_0000+ reaches Game Pak RAM.
        bus.write(0x70_0000, 0xAB);
        assert_eq!(bus.read(0x70_0000), 0xAB);
        assert!(*bus.ram_dirty, "a RAM write marks the buffer dirty");
    }

    #[test]
    fn gsu_bus_drops_rom_writes_and_wraps_the_8mib_space() {
        let rom = [0x11u8, 0x22];
        let mut ram = vec![0u8; 0x8000];
        let mut dirty = false;
        let mut bus = GsuBus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        // A write below the RAM window hits read-only ROM: dropped, not dirty.
        bus.write(0x00_0000, 0xFF);
        assert_eq!(bus.read(0), 0x11, "ROM unchanged by a write");
        assert!(!*bus.ram_dirty, "a ROM write never dirties the battery");
        // Bank $80+ wraps back into the 8 MiB space (RAMBR-style `& 0x7F_FFFF`).
        bus.write(0x70_0010, 0x5A);
        assert_eq!(bus.read(0xF0_0010), 0x5A, "$F0_0010 wraps to $70_0010");
    }

    #[test]
    fn gsu_rom_offset_matches_superfx_lorom_hirom_map() {
        // LoROM ($00-3F): 32 KiB/bank, both bank halves alias the same chunk.
        assert_eq!(gsu_rom_offset(0x00_0000), 0x00_0000);
        assert_eq!(gsu_rom_offset(0x00_7FFF), 0x00_7FFF);
        // $00:8000 (high half) aliases $00:0000.
        assert_eq!(gsu_rom_offset(0x00_8000), 0x00_0000);
        // The Star Fox dead-loop location: GSU $01:003f -> ROM $803f, NOT $1003f.
        assert_eq!(gsu_rom_offset(0x01_003F), 0x00_803F);
        assert_eq!(
            gsu_rom_offset(0x01_8000),
            0x00_8000,
            "high half aliases low"
        );
        assert_eq!(gsu_rom_offset(0x3F_FFFF), 0x1F_FFFF, "top LoROM byte");
        // Linear/HiROM ($40-5F): 64 KiB/bank, same image from offset 0.
        assert_eq!(gsu_rom_offset(0x40_0000), 0x00_0000);
        assert_eq!(gsu_rom_offset(0x41_0000), 0x01_0000);
        assert_eq!(gsu_rom_offset(0x5F_FFFF), 0x1F_FFFF, "top linear byte");
    }

    #[test]
    fn gsu_bus_reads_rom_through_the_lorom_map_not_flat() {
        // A 2 MiB ROM where each 32 KiB LoROM chunk starts with its bank index,
        // so a flat read and a LoROM read return different bytes.
        let mut rom = vec![0u8; 0x20_0000];
        rom[0x00_0000] = 0x5A; // ROM 0, reached by GSU $40:0000 (HiROM view)
        rom[0x00_803F] = 0xC0; // LoROM target for GSU $01:003f
        rom[0x01_003F] = 0xBA; // what a flat map would (wrongly) return
        let mut ram = vec![0u8; 0x8000];
        let mut dirty = false;
        let mut bus = GsuBus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        assert_eq!(
            bus.read(0x01_003F),
            0xC0,
            "GSU bank $01:003f must read the LoROM byte at $803f, not the flat $1003f"
        );
        // The $8000-FFFF half of bank $01 aliases the same LoROM chunk.
        assert_eq!(bus.read(0x01_803F), 0xC0);
        // The HiROM view ($40-5F) of the same ROM returns ROM offset 0.
        assert_eq!(bus.read(0x40_0000), 0x5A, "linear bank $40 aliases ROM 0");
    }

    #[test]
    fn peek_bus_reads_but_never_writes() {
        let rom = [0xAAu8, 0xBB];
        let ram = vec![0x5Au8; 0x8000];
        let mut peek = GsuPeekBus {
            rom: &rom,
            ram: &ram,
        };
        assert_eq!(peek.read(0), 0xAA);
        assert_eq!(peek.read(0x70_0000), 0x5A);
        // Writes are inert (no panic, no mutation path).
        peek.write(0x70_0000, 0x00);
        assert_eq!(peek.read(0x70_0000), 0x5A, "peek write is a no-op");
    }
}
