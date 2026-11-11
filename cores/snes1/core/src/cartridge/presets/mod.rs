//! Preset map-plan generators (LoROM, HiROM, ExHiROM, and the S-DD1,
//! SPC7110, SA-1, Super FX and Cx4 boards).
//!
//! Each preset is a pure function that returns the slot list a stock
//! cartridge of that type would present to the bus. Most take
//! `(rom: &[u8], sram_size: usize, fastrom: bool) -> MapPlan`; the
//! Super FX and Cx4 presets take no FastROM flag, and the SPC7110
//! preset also takes the unpadded ROM length and an RTC flag. The
//! shape mirrors the official Nintendo layouts: a primary ROM window,
//! one or more mirror windows (FastROM-eligible on the upper-half
//! mirrors), and an optional SRAM slot when `sram_size > 0`.
//!
//! Each layout lives in its own file so adding new mappers
//! (ExLoROM mode 22, ...) does not force every preset to fight over
//! one large file. Cross-preset shared logic (the SRAM mirror mask,
//! the two SRAM windows of the HiROM boards) stays here, in
//! `presets::mod`, alongside [`PresetKind`].
//!
//! Presets always cap themselves to "stock" behaviour. Anything
//! custom (banked coprocessor windows, weird mirror exceptions,
//! Sufami Turbo-style two-cart chains) belongs in a manifest
//! (`super::manifest`) where the user explicitly opts into the
//! layout instead of the detector picking it.

use std::ops::RangeInclusive;

use crate::cartridge::{BusSpeed, MapSlot, MapTarget};

mod cx4;
mod exhirom;
mod hirom;
mod lorom;
mod sa1;
mod sdd1;
mod spc7110;
mod superfx;

pub use cx4::cx4_preset;
pub use exhirom::exhirom_preset;
pub use hirom::hirom_preset;
pub use lorom::lorom_preset;
pub use sa1::sa1_preset;
pub use sdd1::sdd1_preset;
pub use spc7110::spc7110_preset;
pub use superfx::superfx_preset;

/// Mirror mask for an SRAM slot of `sram_size` bytes.
///
/// Cartridge SRAM is smaller than the address window it sits behind
/// (a 2 KiB chip lives behind HiROM's 8 KiB-per-bank `$6000-$7FFF`
/// window spread across 32 banks), and the chip's address lines
/// wrap, so the buffer mirrors throughout the whole window. The bus
/// reproduces this by AND-ing the slot-relative offset with
/// `size - 1` before indexing SRAM (checked against bsnes-plus,
/// which wraps the same way). Header SRAM sizes are always powers of
/// two; `next_power_of_two` keeps the mask well-defined for
/// hand-authored manifest sizes that are not.
///
/// Without it, a 2 KiB SRAM only answers in the first 2 KiB of the
/// window and reads zero everywhere else, which trips Donkey Kong
/// Country's anti-piracy check (it writes a marker and reads it back
/// from a mirrored address, expecting the wrapped value).
fn sram_mirror_mask(sram_size: usize) -> u32 {
    debug_assert!(sram_size > 0, "callers gate the SRAM slot on size > 0");
    (sram_size.next_power_of_two() as u32).wrapping_sub(1)
}

/// The battery RAM of a HiROM or ExHiROM board, as its two windows:
/// `$20-$3F:$6000-$7FFF` and `$A0-$BF:$6000-$7FFF`.
///
/// Each bank holds an 8 KiB window of the chip: bank `$20` its first
/// 8 KiB, bank `$21` the next, and so on, folded to the chip's size by
/// [`sram_mirror_mask`]. The board does not decode address line A23 for
/// the RAM, so a bank in `$A0-$BF` reaches the same bytes as the bank
/// 128 below it (`$A1:6000` is `$21:6000`). snes9x
/// (`map_HiROMSRAM`) and the bsnes board files
/// (`20-3f,a0-bf:6000-7fff`) map both halves the same way. Without the
/// second window, a game that writes its RAM through `$20-$3F` and reads
/// it back through `$A0-$BF` reads open bus.
fn hirom_sram_slots(sram_size: usize) -> [MapSlot; 2] {
    let window = |banks: RangeInclusive<u8>| {
        MapSlot::new(
            banks,
            0x6000..=0x7FFF,
            MapTarget::Sram { base_offset: 0 },
            BusSpeed::Slow,
        )
        .with_mask(sram_mirror_mask(sram_size))
    };
    [window(0x20..=0x3F), window(0xA0..=0xBF)]
}

/// Which preset a cartridge belongs to. `Custom` covers anything
/// that does not match one of the other eight layouts, chosen
/// explicitly by a manifest, never returned by `detect_preset`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PresetKind {
    /// 32 KiB-per-bank ROM in the upper-half of every bank
    /// (`$xx:8000-$FFFF`). Map mode `$20`.
    LoRom,
    /// 64 KiB-per-bank ROM in `$C0-$FF`, mirrored upper-half in
    /// `$00-$3F`. Map mode `$21`.
    HiRom,
    /// HiROM extended to two 4 MiB halves, the second living in
    /// `$40-$7D:0000-FFFF` at chip-rom offset `$400000`. Map mode
    /// `$25`.
    ExHiRom,
    /// LoROM program map plus the S-DD1 windows: a static linear ROM
    /// window (`$60-$7F`), the MMC ROM window (`$C0-$FF`, chip-routed),
    /// and the chip registers (`$4800-$480F`). Header identifier
    /// `0x4332` (SFA2) / `0x4532` (Star Ocean).
    Sdd1,
    /// HiROM-style program map plus the SPC7110 windows: shadow PRG
    /// (`$00-0F`/`$80-BF:8000-FFFF`), linear PRG (`$C0-CF`), the bank-remapped
    /// MCU data-ROM window (`$D0-FF`, chip-routed), the DCU decompress port
    /// (`$50`/`$58`), write-gated SRAM (`$6000-7FFF`), the chip registers
    /// (`$4800-483F`), and, for RTC carts, the clock ports (`$4840-4842`).
    /// Header identifier `0xF53A` (SPL4 / Momotarou) / `0xF93A` (Tengai
    /// Makyou Zero, with the RTC-4513).
    Spc7110,
    /// LoROM-located program map plus the SA-1 windows: the Super MMC ROM
    /// windows (`$00-3F`/`$80-BF:8000-FFFF` + `$C0-FF:0000-FFFF`, chip-routed),
    /// I-RAM (`$3000-37FF`), BW-RAM (`$6000-7FFF` window + `$40-4F` linear), and
    /// the chip registers (`$2200-23FF`). Header `map_mode $23` with a
    /// `cartridge_type` in `{0x32, 0x34, 0x35, 0x36}` (Super Mario RPG, Kirby
    /// Super Star, Kirby's Dream Land 3, Marvelous, Parodius, PGA Tour 96).
    Sa1,
    /// LoROM-located program map plus the Super FX (GSU) windows: ROM
    /// (`$00-3F`/`$80-BF:8000-FFFF` LoROM + `$40-5F`/`$C0-DF:0000-FFFF`
    /// linear, chip-routed), the chip registers (`$3000-34FF`, chip-routed),
    /// and, when the cart declares it, Game Pak RAM (`$6000-7FFF` peephole
    /// and `$60-7D`/`$E0-FF` linear, chip-routed). Header `map_mode $20`
    /// (identical to LoROM) with a `cartridge_type` in
    /// `{0x13, 0x14, 0x15, 0x1A}` (Star Fox, Doom, Stunt Race FX, Yoshi's
    /// Island).
    SuperFx,
    /// LoROM-located program map plus the Cx4 (Hitachi HG51B169) windows: cart
    /// ROM (`$00-3F`/`$80-BF:8000-FFFF` LoROM half-banks, chip-routed), the
    /// MMIO + internal data RAM window (`$00-3F`/`$80-BF:6000-7FFF`,
    /// chip-routed), and, only when a manifest declares it, cart RAM
    /// (`$70-77:0000-7FFF`, chip-routed). Header `map_mode $20` (identical to
    /// LoROM) with `cartridge_type $F3` (Mega Man X2, Mega Man X3).
    Cx4,
    /// Slots come from a manifest, not from one of the stock
    /// generators.
    Custom,
}
