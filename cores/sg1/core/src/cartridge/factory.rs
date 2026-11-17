//! Genesis chip factory: manifest `kind` -> concrete [`Coprocessor`].
//!
//! [`Sg1ChipFactory`] is the Genesis implementation of the generic
//! [`ChipFactory`] seam. The generic `build(kind, chip_id, rom,
//! master_clock_hz)` signature carries everything the SVP and the bank
//! mappers need (they only want the cartridge ROM); the one per-build input
//! it can't express, a serial EEPROM's byte size, lives on the factory
//! struct, which the [`builder`](super::builder) constructs fresh for each cartridge.
//!
//! Wired kinds: `"sega"` / `"ssf2"` / `"codemasters"` (the
//! [`BankMapperChip`]), `"eeprom"` (the [`EepromChip`]), `"jcart"` (the
//! [`JCartChip`]), and `"svp"` (the [`SvpCoprocessor`]). An unknown kind
//! returns `None` so the builder rejects the manifest loudly.

use std::sync::Arc;

use retro_core::cartridge::{ChipFactory, Coprocessor};

use super::chips::{BankMapperChip, EepromChip, JCartChip, SvpCoprocessor};
use super::savestate::BankMapperKind;

/// Default serial-EEPROM size (X24C01, 128 bytes), the size the builder gives
/// every EEPROM cart.
pub const DEFAULT_EEPROM_SIZE: usize = 128;

/// Genesis [`ChipFactory`]: builds the console's coprocessors from a
/// manifest `kind`, carrying the EEPROM size the generic signature omits.
pub struct Sg1ChipFactory {
    /// Serial-EEPROM byte size (128 or 256) for the `"eeprom"` kind.
    pub eeprom_size: usize,
}

impl Default for Sg1ChipFactory {
    fn default() -> Self {
        Self {
            eeprom_size: DEFAULT_EEPROM_SIZE,
        }
    }
}

impl ChipFactory for Sg1ChipFactory {
    fn build(
        &self,
        kind: &str,
        _chip_id: u8,
        rom: Arc<[u8]>,
        master_clock_hz: u64,
    ) -> Option<Box<dyn Coprocessor>> {
        match kind.to_ascii_lowercase().as_str() {
            "sega" => Some(Box::new(BankMapperChip::new(rom, BankMapperKind::Sega))),
            "ssf2" => Some(Box::new(BankMapperChip::new(rom, BankMapperKind::Ssf2))),
            "codemasters" => Some(Box::new(BankMapperChip::new(
                rom,
                BankMapperKind::Codemasters,
            ))),
            "eeprom" => Some(Box::new(EepromChip::new(self.eeprom_size))),
            "jcart" => Some(Box::new(JCartChip::new())),
            "svp" => Some(Box::new(SvpCoprocessor::new(rom, master_clock_hz))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_HZ: u64 = 53_693_175;

    fn rom() -> Arc<[u8]> {
        Arc::from(vec![0u8; 0x200000].into_boxed_slice())
    }

    #[test]
    fn builds_each_known_kind() {
        let f = Sg1ChipFactory::default();
        assert_eq!(
            f.build("sega", 0, rom(), NTSC_HZ).unwrap().label(),
            "SEGA mapper"
        );
        assert_eq!(
            f.build("ssf2", 0, rom(), NTSC_HZ).unwrap().label(),
            "SSF2 mapper"
        );
        assert_eq!(
            f.build("codemasters", 0, rom(), NTSC_HZ).unwrap().label(),
            "Codemasters mapper"
        );
        assert_eq!(
            f.build("eeprom", 0, rom(), NTSC_HZ).unwrap().label(),
            "Serial EEPROM"
        );
        assert_eq!(
            f.build("jcart", 0, rom(), NTSC_HZ).unwrap().label(),
            "J-Cart"
        );
        assert_eq!(f.build("svp", 0, rom(), NTSC_HZ).unwrap().label(), "SVP");
    }

    #[test]
    fn kind_is_case_insensitive() {
        let f = Sg1ChipFactory::default();
        assert!(f.build("SEGA", 0, rom(), NTSC_HZ).is_some());
        assert!(f.build("SvP", 0, rom(), NTSC_HZ).is_some());
    }

    #[test]
    fn eeprom_size_honoured() {
        let f = Sg1ChipFactory { eeprom_size: 256 };
        let chip = f.build("eeprom", 0, rom(), NTSC_HZ).unwrap();
        assert_eq!(chip.battery().unwrap().len(), 256);
    }

    #[test]
    fn unknown_kind_returns_none() {
        let f = Sg1ChipFactory::default();
        assert!(f.build("nonsense", 0, rom(), NTSC_HZ).is_none());
    }
}
