//! The SNES's cartridges.
//!
//! A cartridge is retro-core's composable cartridge
//! ([`retro_core::cartridge::ComposableCartridge`]) over this console's
//! board ([`SnesCartridge`]): the ROM, an optional battery SRAM, a
//! [`MapPlan`] the 65C816's address space resolves through, and a chip
//! set of coprocessors (the DSP-1, the S-DD1, the SPC7110, the SA-1, the
//! Super FX, the Cx4). The bus calls [`SnesBus::install_cartridge`] once
//! and from then on every access is dispatched through the cartridge.
//!
//! What this core keeps is what the SNES makes different: a slot is a
//! bank by offset rectangle with an access speed ([`MapPlan`],
//! [`MapSlot`], [`BusSpeed`]); the board ([`SnesBoard`]); the header
//! parsing and the preset map-plan generators; the manifest's slot and
//! presets; the chips ([`chips`]) and the factory that builds them; and
//! the [`CartridgeBuilder`] that strips a copier header, pads, detects and
//! assembles a cartridge.
//!
//! [`SnesBus::install_cartridge`]: crate::bus::SnesBus::install_cartridge

mod board;
mod builder;
pub mod chips;
mod detect;
mod factory;
mod header;
mod manifest;
mod map_plan;
mod presets;

#[cfg(test)]
pub(crate) use board::{test_cartridge, test_cartridge_with_chips};
pub use board::{SnesBoard, SnesCartridge};
pub use builder::{copier_header_length, detected_manifest, BuildError, CartridgeBuilder};
#[cfg(feature = "chip-cx4")]
pub use chips::cx4::Cx4Chip;
pub use chips::dsp1::{
    detect_dsp1_layout, CoprocessorMode, Dsp1Chip, Dsp1Layout, DSP1_CHIP_ID, NEC_LLE_BUILT,
};
pub use chips::dsp1_hle::Dsp1Hle;
pub use chips::firmware::{ByteOrder, Dsp1Firmware, Dsp1Revision, FirmwareError, FIRMWARE_SIZE};
#[cfg(feature = "chip-sa1")]
pub use chips::sa1::Sa1Chip;
pub use chips::sdd1::{Sdd1Chip, SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE};
pub use chips::spc7110::{
    fixed_unix_time, set_fixed_unix_time, Spc7110Chip, SPC7110_CHIP_ID, SPC7110_DCU_BASE,
    SPC7110_MCU_BASE, SPC7110_REG_BASE, SPC7110_SRAM_BASE,
};
#[cfg(feature = "chip-superfx")]
pub use chips::superfx::SuperFxChip;
pub use chips::support::{
    unsupported_chip, unsupported_preset, CHIP_CX4_BUILT, CHIP_SA1_BUILT, CHIP_SUPERFX_BUILT,
};
pub use chips::{
    CX4_CHIP_ID, CX4_MMIO_BASE, CX4_RAM_BASE, CX4_ROM_BASE, SA1_BWRAM_BASE, SA1_BWRAM_LINEAR_BASE,
    SA1_CHIP_ID, SA1_IRAM_BASE, SA1_MMIO_BASE, SA1_ROM_BASE, SUPERFX_CHIP_ID, SUPERFX_MMIO_BASE,
    SUPERFX_RAM_BASE, SUPERFX_ROM_BASE,
};
pub use detect::{
    detect_preset, detect_preset_with_threshold, score_header, DetectedPreset,
    DEFAULT_DETECT_THRESHOLD,
};
pub use header::{
    try_parse_header, SnesHeader, SnesVectors, HEADER_CANDIDATES, MAP_MODE_EXHIROM, MAP_MODE_HIROM,
    MAP_MODE_LOROM,
};
pub use manifest::{
    manifest_to_map_plan, parse_manifest, CartridgeManifest, ManifestChip, ManifestControllers,
    ManifestError, ManifestMapping, ManifestRom, ManifestSlot, ManifestTarget, SnesManifestExtras,
    SnesMappingKnobs, SUPPORTED_MANIFEST_VERSION,
};
pub use map_plan::{BusSpeed, MapPlan, MapSlot, MapTarget, Resolved};
pub use presets::{
    cx4_preset, exhirom_preset, hirom_preset, lorom_preset, sa1_preset, sdd1_preset,
    spc7110_preset, superfx_preset, PresetKind,
};
