//! The chip ids and chip-address bases of the SA-1, the Super FX and the
//! Cx4: what a map plan carries for a cartridge window the chip serves.
//!
//! They live apart from the chip modules so that the presets, the bus and
//! the builder can name them in a build that leaves a chip's module out
//! (its `chip-*` feature off): such a cartridge is refused at load, but the
//! code that would have mapped it still compiles.

/// `chip_id` the bus dispatch carries for the SA-1. Distinct from the DSP-1
/// (id 1), S-DD1 (id 5), and SPC7110 (id 6) so a manifest can place each at
/// its own slot.
pub const SA1_CHIP_ID: u8 = 7;

/// Chip-address base for the **MMIO** register window (`$2200-$23FF`). A
/// register access arrives as `SA1_MMIO_BASE + (reg - 0x2200)`, so the chip
/// recovers the `$22xx`/`$23xx` address by adding `0x2200`.
pub const SA1_MMIO_BASE: u32 = 0x1000_0000;

/// Chip-address base for the S-CPU **I-RAM** window (`$3000-$37FF`). The slot
/// folds the bank away, so an access arrives as `SA1_IRAM_BASE + (0..0x7FF)`.
pub const SA1_IRAM_BASE: u32 = 0x2000_0000;

/// Chip-address base for the **BW-RAM 8 KiB window** (`$6000-$7FFF`), offset
/// inside BW-RAM by the `$2224` block select.
pub const SA1_BWRAM_BASE: u32 = 0x3000_0000;

/// Chip-address base for the **linear BW-RAM** view (`$40-$4F:0000-FFFF`).
pub const SA1_BWRAM_LINEAR_BASE: u32 = 0x4000_0000;

/// Chip-address base for the **Super MMC ROM** windows. Each ROM slot carries
/// `base = SA1_ROM_BASE + window_start`, so `base + compute_target_offset`
/// reconstructs `SA1_ROM_BASE + addr24`, the chip subtracts the base to get
/// the full 24-bit address and applies the Super MMC per bank. Highest
/// sentinel, so a single descending magnitude test splits the five windows.
pub const SA1_ROM_BASE: u32 = 0x5000_0000;

/// `chip_id` the bus dispatch carries for the Super FX. Distinct from the
/// DSP-1 (id 1), S-DD1 (id 5), SPC7110 (id 6), and SA-1 (id 7) so a manifest
/// can place each at its own slot.
pub const SUPERFX_CHIP_ID: u8 = 8;

/// Chip-address base for the **MMIO** register window (`$3000-$34FF`). A
/// register access arrives as `SUPERFX_MMIO_BASE + (reg - 0x3000)`, so the
/// chip recovers the `$30xx`/`$31xx` address by adding `0x3000`. Spaced far
/// above the window width so a window-relative offset can never reach
/// [`SUPERFX_RAM_BASE`].
pub const SUPERFX_MMIO_BASE: u32 = 0x1000_0000;

/// Chip-address base for the **Game Pak RAM** windows. The chip subtracts the
/// base and folds the result modulo the RAM size, so the 8 KiB `$6000-7FFF`
/// peephole and the `$60-7D`/`$E0-FF` linear views all land in the same
/// buffer.
pub const SUPERFX_RAM_BASE: u32 = 0x2000_0000;

/// Chip-address base for the **Game Pak ROM** windows. The ROM slots keep the
/// plain-`Rom` geometry (LoROM `$8000-FFFF` half-banks + `$40-5F` linear), so
/// `offset - SUPERFX_ROM_BASE` is exactly the ROM byte offset a `Rom` slot
/// would have produced; routing it through the chip only lets RON open-bus
/// arbitration intercept the read while the GSU runs. Spaced above
/// [`SUPERFX_RAM_BASE`] and clear of the largest ROM offset (`0x1F_FFFF`).
pub const SUPERFX_ROM_BASE: u32 = 0x3000_0000;

/// `chip_id` the bus dispatch carries for the Cx4. Distinct from the DSP-1
/// (id 1), S-DD1 (id 5), SPC7110 (id 6), SA-1 (id 7), and Super FX (id 8) so a
/// manifest can place each at its own slot.
pub const CX4_CHIP_ID: u8 = 9;

/// Chip-address base for the **MMIO + internal data RAM** window
/// (`$6000-7FFF`). An access arrives as `CX4_MMIO_BASE + (addr & 0x1FFF)`, so
/// the chip recovers the `$6000-7FFF` SNES offset by OR-ing `0x6000`. Spaced
/// far above the window width so a window-relative offset can never reach
/// [`CX4_RAM_BASE`].
pub const CX4_MMIO_BASE: u32 = 0x1000_0000;

/// Chip-address base for the **cart RAM** window (`$70-77:0000-7FFF`). The chip
/// subtracts the base and folds the result modulo the RAM size. Present only
/// when a manifest declares SRAM (`ram_size > 0`).
pub const CX4_RAM_BASE: u32 = 0x2000_0000;

/// Chip-address base for the **cart ROM** windows (LoROM `$8000-FFFF`). The ROM
/// slot keeps plain-`Rom` geometry, so `offset - CX4_ROM_BASE` is exactly the
/// ROM byte offset a `Rom` slot would have produced; routing it through the
/// chip only lets bus-contention interpose while the HG51B169 runs. Spaced
/// above [`CX4_RAM_BASE`] and clear of the largest ROM offset (`0x1F_FFFF`).
pub const CX4_ROM_BASE: u32 = 0x3000_0000;
