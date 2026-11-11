//! Which coprocessors this build carries.
//!
//! The SA-1, the Super FX and the Cx4 each sit behind a Cargo feature
//! (`chip-sa1`, `chip-superfx`, `chip-cx4`, all on by default), so a build
//! can leave a chip's module out. A cartridge that needs a chip the build
//! has no module for is refused at load with
//! [`BuildError::UnsupportedChip`](super::BuildError::UnsupportedChip)
//! naming the chip, which a host maps to another core.

use super::PresetKind;

/// Whether this build carries the SA-1 (the `chip-sa1` feature).
pub const CHIP_SA1_BUILT: bool = cfg!(feature = "chip-sa1");
/// Whether this build carries the Super FX (the `chip-superfx` feature).
pub const CHIP_SUPERFX_BUILT: bool = cfg!(feature = "chip-superfx");
/// Whether this build carries the Cx4 (the `chip-cx4` feature).
pub const CHIP_CX4_BUILT: bool = cfg!(feature = "chip-cx4");

/// The display name of the chip a manifest `kind` names when this build
/// has no module for it; `None` for a chip that is built in, and for a
/// kind the console does not know (the factory reports that one).
pub fn unsupported_chip(kind: &str) -> Option<&'static str> {
    match kind.to_ascii_lowercase().as_str() {
        "sa1" if !CHIP_SA1_BUILT => Some("SA-1"),
        "superfx" if !CHIP_SUPERFX_BUILT => Some("Super FX"),
        "cx4" if !CHIP_CX4_BUILT => Some("Cx4"),
        _ => None,
    }
}

/// The same question for a detected mapping: the chip the preset implies.
pub fn unsupported_preset(kind: PresetKind) -> Option<&'static str> {
    match kind {
        PresetKind::Sa1 => unsupported_chip("sa1"),
        PresetKind::SuperFx => unsupported_chip("superfx"),
        PresetKind::Cx4 => unsupported_chip("cx4"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_chip_is_supported_and_an_unbuilt_one_is_named() {
        assert_eq!(unsupported_chip("sa1").is_none(), CHIP_SA1_BUILT);
        assert_eq!(unsupported_chip("SuperFX").is_none(), CHIP_SUPERFX_BUILT);
        assert_eq!(unsupported_chip("cx4").is_none(), CHIP_CX4_BUILT);
        assert_eq!(unsupported_chip("dsp1"), None);
        assert_eq!(unsupported_chip("nonsense"), None);
        assert_eq!(unsupported_preset(PresetKind::LoRom), None);
        assert_eq!(
            unsupported_preset(PresetKind::Sa1).is_none(),
            CHIP_SA1_BUILT
        );
        assert_eq!(
            unsupported_preset(PresetKind::SuperFx).is_none(),
            CHIP_SUPERFX_BUILT
        );
        assert_eq!(
            unsupported_preset(PresetKind::Cx4).is_none(),
            CHIP_CX4_BUILT
        );
    }

    #[cfg(not(feature = "chip-sa1"))]
    #[test]
    fn without_the_sa1_its_name_is_the_one_a_host_shows() {
        assert_eq!(unsupported_chip("sa1"), Some("SA-1"));
        assert_eq!(unsupported_preset(PresetKind::Sa1), Some("SA-1"));
    }
}
