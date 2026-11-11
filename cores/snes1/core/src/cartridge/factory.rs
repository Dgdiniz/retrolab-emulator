//! Console chip factory: manifest `kind` -> concrete [`Coprocessor`].
//!
//! [`Snes1ChipFactory`] is the snes1 implementation of the generic
//! [`ChipFactory`] seam. The generic `build(kind, chip_id, rom,
//! master_clock_hz)` signature can't carry the DSP-1's firmware blob,
//! emulation mode, or DR/SR-select bit, so those per-build inputs live on the
//! factory struct itself, it is console-owned build context that the
//! [`CartridgeBuilder`](super::CartridgeBuilder) constructs fresh each build.
//!
//! The wired kinds are the DSP-1 (`"dsp1"` and its revision spellings), the
//! S-DD1 (`"sdd1"`), the SPC7110 (`"spc7110"`), the SA-1 (`"sa1"`), the
//! Super FX (`"superfx"`) and the Cx4 (`"cx4"`); an unknown kind returns
//! `None` so the builder rejects the manifest loudly. The S-DD1 needs only
//! the ROM `Arc<[u8]>` the generic signature provides: an MMC / decompressor
//! chip needs it and nothing else, so it carries no extra factory state. A
//! new console chip adds an arm here. The SA-1, Super FX and Cx4 arms exist
//! only with their `chip-*` feature; the builder refuses such a cartridge
//! before asking (`chip_support`).

use std::sync::Arc;

use retro_core::cartridge::{ChipFactory, Coprocessor};

#[cfg(feature = "chip-cx4")]
use super::cx4::Cx4Chip;
use super::dsp1::{is_dsp1_kind, select_dsp1_chip, CoprocessorMode, Dsp1Layout};
#[cfg(feature = "chip-sa1")]
use super::sa1::Sa1Chip;
use super::sdd1::Sdd1Chip;
use super::spc7110::Spc7110Chip;
#[cfg(feature = "chip-superfx")]
use super::superfx::SuperFxChip;

/// SPC7110 build context the generic `build` signature can't carry: the
/// unpadded ROM length (for `datarom_addr` wrapping), the battery SRAM size,
/// and whether the cart carries the RTC-4513. Resolved by the
/// [`CartridgeBuilder`](super::CartridgeBuilder) from the header/manifest.
#[derive(Clone, Copy, Default)]
pub(crate) struct Spc7110Config {
    /// Unpadded ROM length in bytes.
    pub real_rom_len: usize,
    /// Battery SRAM size in bytes (0 for none).
    pub sram_size: usize,
    /// Whether the cart carries the RTC-4513 (`$F9` header).
    pub has_rtc: bool,
}

/// SA-1 build context the generic `build` signature can't carry: the BW-RAM
/// size and whether that RAM is battery-backed. Resolved by the
/// [`CartridgeBuilder`](super::CartridgeBuilder) from the header/manifest.
#[derive(Clone, Copy, Default)]
pub(crate) struct Sa1Config {
    /// BW-RAM size in bytes (0 for none).
    pub bwram_size: usize,
    /// Whether BW-RAM is battery-backed (`$34`/`$35` carts).
    pub has_battery: bool,
}

/// Super FX build context the generic `build` signature can't carry: the Game
/// Pak RAM size (already resolved through the Star Fox quirk) and whether that
/// RAM is battery-backed. Resolved by the
/// [`CartridgeBuilder`](super::CartridgeBuilder) from the header/manifest.
#[derive(Clone, Copy, Default)]
pub(crate) struct SuperFxConfig {
    /// Game Pak RAM size in bytes (0 for none).
    pub ram_size: usize,
    /// Whether Game Pak RAM is battery-backed (`$15` carts, e.g. Yoshi).
    pub has_battery: bool,
}

/// Cx4 build context the generic `build` signature can't carry: the cart RAM
/// size and whether that RAM is battery-backed. The real MMX2/X3 carts declare
/// no SRAM, so both default to "none"; a manifest may declare battery-backed
/// SRAM (`ram_size > 0` => `has_battery`). Resolved by the
/// [`CartridgeBuilder`](super::CartridgeBuilder).
#[derive(Clone, Copy, Default)]
pub(crate) struct Cx4Config {
    /// Cart RAM size in bytes (0 for none, the real MMX2/X3 case).
    pub ram_size: usize,
    /// Whether cart RAM is battery-backed (manifest-declared `ram_size > 0`).
    pub has_battery: bool,
}

/// snes1's [`ChipFactory`]: builds the console's concrete coprocessors from a
/// manifest `kind`, carrying the DSP-1 build context the generic signature
/// can't express.
pub(crate) struct Snes1ChipFactory {
    /// Coprocessor firmware blob (e.g. an 8192-byte DSP-1 dump), when staged.
    pub firmware: Option<Vec<u8>>,
    /// How a detected coprocessor is emulated (Auto / LLE / HLE).
    pub mode: CoprocessorMode,
    /// The resolved DSP-1 register-window layout, present iff the build has a
    /// DSP-1. Supplies the DR/SR-select bit the generic signature omits.
    pub dsp1_layout: Option<Dsp1Layout>,
    /// SPC7110 build context (ROM length / SRAM size / RTC), consulted only
    /// when building the `"spc7110"` kind.
    pub spc7110: Spc7110Config,
    /// SA-1 build context (BW-RAM size / battery), consulted only when building
    /// the `"sa1"` kind.
    pub sa1: Sa1Config,
    /// Super FX build context (Game Pak RAM size / battery), consulted only
    /// when building the `"superfx"` kind.
    pub superfx: SuperFxConfig,
    /// Cx4 build context (cart RAM size / battery), consulted only when
    /// building the `"cx4"` kind.
    pub cx4: Cx4Config,
}

impl ChipFactory for Snes1ChipFactory {
    fn build(
        &self,
        kind: &str,
        _chip_id: u8,
        rom: Arc<[u8]>,
        master_clock_hz: u64,
    ) -> Option<Box<dyn Coprocessor>> {
        if is_dsp1_kind(kind) {
            // The DSP-1 ignores the cartridge ROM (its firmware is separate);
            // the DR/SR-select bit comes from the resolved layout. A DSP-1
            // kind with no resolved layout can't be built, so report unknown.
            let sr_bit = self.dsp1_layout?.sr_select_bit();
            return Some(select_dsp1_chip(
                self.firmware.as_deref(),
                self.mode,
                sr_bit,
                master_clock_hz,
            ));
        }
        if kind.eq_ignore_ascii_case("sdd1") {
            // The S-DD1 owns a clone of the cartridge ROM for the MMC and the
            // decompressor. No firmware, no layout bit.
            return Some(Box::new(Sdd1Chip::new(rom, master_clock_hz)));
        }
        if kind.eq_ignore_ascii_case("spc7110") {
            // The SPC7110 owns a clone of the cartridge ROM for the MCU
            // data-ROM window, the decompressor and the data port. The
            // unpadded length, SRAM size, and RTC flag ride on the factory.
            let cfg = self.spc7110;
            return Some(Box::new(Spc7110Chip::new(
                rom,
                cfg.real_rom_len,
                cfg.sram_size,
                cfg.has_rtc,
                master_clock_hz,
            )));
        }
        if kind.eq_ignore_ascii_case("sa1") {
            return self.build_sa1(rom, master_clock_hz);
        }
        if kind.eq_ignore_ascii_case("superfx") {
            return self.build_superfx(rom, master_clock_hz);
        }
        if kind.eq_ignore_ascii_case("cx4") {
            return self.build_cx4(rom, master_clock_hz);
        }
        // Unknown to this console.
        None
    }
}

impl Snes1ChipFactory {
    /// The SA-1 owns a clone of the cartridge ROM for the Super MMC; the
    /// BW-RAM size and battery flag ride on the factory.
    #[cfg(feature = "chip-sa1")]
    fn build_sa1(&self, rom: Arc<[u8]>, master_clock_hz: u64) -> Option<Box<dyn Coprocessor>> {
        let cfg = self.sa1;
        Some(Box::new(Sa1Chip::new(
            rom,
            cfg.bwram_size,
            cfg.has_battery,
            master_clock_hz,
        )))
    }

    /// Without the `chip-sa1` feature there is no SA-1 to build; the builder
    /// refuses the cartridge before asking.
    #[cfg(not(feature = "chip-sa1"))]
    fn build_sa1(&self, rom: Arc<[u8]>, master_clock_hz: u64) -> Option<Box<dyn Coprocessor>> {
        let _ = (
            self.sa1.bwram_size,
            self.sa1.has_battery,
            rom,
            master_clock_hz,
        );
        None
    }

    /// The Super FX owns a clone of the cartridge ROM for the GSU bus; the
    /// Game Pak RAM size and battery flag ride on the factory.
    #[cfg(feature = "chip-superfx")]
    fn build_superfx(&self, rom: Arc<[u8]>, master_clock_hz: u64) -> Option<Box<dyn Coprocessor>> {
        let cfg = self.superfx;
        Some(Box::new(SuperFxChip::new(
            rom,
            cfg.ram_size,
            cfg.has_battery,
            master_clock_hz,
        )))
    }

    /// Without the `chip-superfx` feature there is no Super FX to build.
    #[cfg(not(feature = "chip-superfx"))]
    fn build_superfx(&self, rom: Arc<[u8]>, master_clock_hz: u64) -> Option<Box<dyn Coprocessor>> {
        let _ = (
            self.superfx.ram_size,
            self.superfx.has_battery,
            rom,
            master_clock_hz,
        );
        None
    }

    /// The Cx4 owns a clone of the cartridge ROM for the HG51B169 bus; the
    /// cart RAM size and battery flag ride on the factory (both 0/false for
    /// the real MMX2/X3 carts).
    #[cfg(feature = "chip-cx4")]
    fn build_cx4(&self, rom: Arc<[u8]>, master_clock_hz: u64) -> Option<Box<dyn Coprocessor>> {
        let cfg = self.cx4;
        Some(Box::new(Cx4Chip::new(
            rom,
            cfg.ram_size,
            cfg.has_battery,
            master_clock_hz,
        )))
    }

    /// Without the `chip-cx4` feature there is no Cx4 to build.
    #[cfg(not(feature = "chip-cx4"))]
    fn build_cx4(&self, rom: Arc<[u8]>, master_clock_hz: u64) -> Option<Box<dyn Coprocessor>> {
        let _ = (
            self.cx4.ram_size,
            self.cx4.has_battery,
            rom,
            master_clock_hz,
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_HZ: u64 = 21_477_270;

    fn rom() -> Arc<[u8]> {
        Arc::from(vec![0u8; 16].into_boxed_slice())
    }

    fn factory(firmware: Option<Vec<u8>>, mode: CoprocessorMode) -> Snes1ChipFactory {
        Snes1ChipFactory {
            firmware,
            mode,
            dsp1_layout: Some(Dsp1Layout::HiRom),
            spc7110: Spc7110Config::default(),
            sa1: Sa1Config::default(),
            superfx: SuperFxConfig::default(),
            cx4: Cx4Config::default(),
        }
    }

    #[test]
    fn builds_dsp1_lle_with_firmware_when_the_lle_is_built() {
        // With the `nec-lle` feature the firmware selects the LLE; without
        // it the firmware is ignored and the HLE runs.
        let f = factory(Some(vec![0u8; 8192]), CoprocessorMode::Auto);
        let chip = f.build("dsp1", 1, rom(), NTSC_HZ).expect("dsp1 builds");
        let built = crate::cartridge::NEC_LLE_BUILT;
        assert_eq!(
            chip.label().contains("LLE"),
            built,
            "label = {}",
            chip.label()
        );
        assert_eq!(
            chip.label().contains("HLE"),
            !built,
            "label = {}",
            chip.label()
        );
    }

    #[test]
    fn builds_dsp1_hle_without_firmware() {
        // Auto without firmware installs the firmware-free HLE. The revision
        // spelling "dsp-1b" is also accepted by `is_dsp1_kind`.
        let f = factory(None, CoprocessorMode::Auto);
        let chip = f.build("dsp-1b", 1, rom(), NTSC_HZ).expect("dsp1 builds");
        assert!(chip.label().contains("HLE"), "label = {}", chip.label());
    }

    #[test]
    fn forced_lle_without_firmware_is_idle_stub() {
        let f = factory(None, CoprocessorMode::Lle);
        let chip = f.build("dsp1", 1, rom(), NTSC_HZ).expect("dsp1 builds");
        assert!(chip.label().contains("idle"), "label = {}", chip.label());
    }

    #[test]
    fn builds_sdd1_from_rom() {
        // The S-DD1 builds with no firmware/layout, straight off the ROM Arc.
        let f = factory(None, CoprocessorMode::Auto);
        let chip = f.build("sdd1", 5, rom(), NTSC_HZ).expect("sdd1 builds");
        assert_eq!(chip.label(), "S-DD1");
        // Case-insensitive kind spelling is accepted too.
        assert!(f.build("SDD1", 5, rom(), NTSC_HZ).is_some());
    }

    #[test]
    fn builds_spc7110_from_rom() {
        // The SPC7110 builds off the ROM Arc plus its config; the config's
        // real_rom_len / sram_size / has_rtc flow through to the chip.
        let f = Snes1ChipFactory {
            firmware: None,
            mode: CoprocessorMode::Auto,
            dsp1_layout: None,
            spc7110: Spc7110Config {
                real_rom_len: 16,
                sram_size: 8 * 1024,
                has_rtc: true,
            },
            sa1: Sa1Config::default(),
            superfx: SuperFxConfig::default(),
            cx4: Cx4Config::default(),
        };
        let chip = f
            .build("spc7110", 6, rom(), NTSC_HZ)
            .expect("spc7110 builds");
        assert_eq!(chip.label(), "SPC7110");
        // On an RTC ($F9) cart the battery blob is SRAM + the 20-byte RTC file.
        assert_eq!(chip.battery().map(<[u8]>::len), Some(8 * 1024 + 20));
        // Case-insensitive spelling is accepted too.
        assert!(f.build("SPC7110", 6, rom(), NTSC_HZ).is_some());
    }

    #[cfg(feature = "chip-sa1")]
    #[test]
    fn builds_sa1_from_rom() {
        // The SA-1 builds off the ROM Arc plus its config; the BW-RAM size and
        // battery flag flow through to the chip.
        let f = Snes1ChipFactory {
            firmware: None,
            mode: CoprocessorMode::Auto,
            dsp1_layout: None,
            spc7110: Spc7110Config::default(),
            sa1: Sa1Config {
                bwram_size: 8 * 1024,
                has_battery: true,
            },
            superfx: SuperFxConfig::default(),
            cx4: Cx4Config::default(),
        };
        let chip = f.build("sa1", 7, rom(), NTSC_HZ).expect("sa1 builds");
        assert_eq!(chip.label(), "SA-1");
        // A battery ($35) cart exposes BW-RAM for `.srm` persistence.
        assert_eq!(chip.battery().map(<[u8]>::len), Some(8 * 1024));
        // Case-insensitive spelling is accepted too.
        assert!(f.build("SA1", 7, rom(), NTSC_HZ).is_some());
    }

    #[cfg(feature = "chip-superfx")]
    #[test]
    fn builds_superfx_from_rom() {
        // The Super FX builds off the ROM Arc plus its config; the Game Pak RAM
        // size and battery flag flow through to the chip.
        let f = Snes1ChipFactory {
            firmware: None,
            mode: CoprocessorMode::Auto,
            dsp1_layout: None,
            spc7110: Spc7110Config::default(),
            sa1: Sa1Config::default(),
            superfx: SuperFxConfig {
                ram_size: 0x8000,
                has_battery: true,
            },
            cx4: Cx4Config::default(),
        };
        let chip = f
            .build("superfx", 8, rom(), NTSC_HZ)
            .expect("superfx builds");
        assert_eq!(chip.label(), "Super FX");
        // A battery ($15) cart exposes Game Pak RAM for `.srm` persistence.
        assert_eq!(chip.battery().map(<[u8]>::len), Some(0x8000));
        // Case-insensitive spelling is accepted too.
        assert!(f.build("SuperFX", 8, rom(), NTSC_HZ).is_some());
    }

    #[cfg(feature = "chip-cx4")]
    #[test]
    fn builds_cx4_from_rom() {
        // The Cx4 builds off the ROM Arc plus its config. The real MMX2/X3
        // carts declare no SRAM (ram_size 0 => no battery); a manifest may
        // declare battery-backed SRAM.
        let f = Snes1ChipFactory {
            firmware: None,
            mode: CoprocessorMode::Auto,
            dsp1_layout: None,
            spc7110: Spc7110Config::default(),
            sa1: Sa1Config::default(),
            superfx: SuperFxConfig::default(),
            cx4: Cx4Config {
                ram_size: 0x2000,
                has_battery: true,
            },
        };
        let chip = f.build("cx4", 9, rom(), NTSC_HZ).expect("cx4 builds");
        assert_eq!(chip.label(), "Cx4");
        // A manifest-declared, battery-backed SRAM is exposed for `.srm`.
        assert_eq!(chip.battery().map(<[u8]>::len), Some(0x2000));
        // Case-insensitive spelling is accepted too.
        assert!(f.build("CX4", 9, rom(), NTSC_HZ).is_some());

        // With no cart RAM (the real MMX2/X3 case) there is no battery.
        let f_no_ram = Snes1ChipFactory {
            firmware: None,
            mode: CoprocessorMode::Auto,
            dsp1_layout: None,
            spc7110: Spc7110Config::default(),
            sa1: Sa1Config::default(),
            superfx: SuperFxConfig::default(),
            cx4: Cx4Config::default(),
        };
        let chip = f_no_ram
            .build("cx4", 9, rom(), NTSC_HZ)
            .expect("cx4 builds");
        assert!(chip.battery().is_none());
    }

    #[test]
    fn unknown_kind_returns_none() {
        let f = factory(None, CoprocessorMode::Auto);
        assert!(f.build("nonsense", 0, rom(), NTSC_HZ).is_none());
    }

    #[test]
    fn dsp1_without_layout_returns_none() {
        // No resolved layout => no DR/SR-select bit => can't build a DSP-1.
        let f = Snes1ChipFactory {
            firmware: None,
            mode: CoprocessorMode::Auto,
            dsp1_layout: None,
            spc7110: Spc7110Config::default(),
            sa1: Sa1Config::default(),
            superfx: SuperFxConfig::default(),
            cx4: Cx4Config::default(),
        };
        assert!(f.build("dsp1", 1, rom(), NTSC_HZ).is_none());
    }
}
