//! What the host is told about the console: its CPUs (a cartridge's own
//! CPU among them) and the pad's buttons.

use retro_core::*;

use crate::cartridge::PresetKind;

/// The two CPUs every SNES console has: the 5A22 main CPU (65C816 core) and
/// the SPC700 audio CPU. A cartridge coprocessor that hosts its own steppable
/// CPU (the SA-1, the Super FX or the Cx4) appends a third slot on top of this
/// base in [`EmulatorCore::load_rom`].
pub(super) fn snes_base_cpus() -> Vec<CpuInfo> {
    vec![
        CpuInfo {
            id: 0,
            name: "Ricoh 5A22 (WDC 65C816)".to_string(),
            architecture: CpuArchitecture::Mos65816,
            clock_speed: 3_580_000,
            location: CpuLocation::Motherboard,
            address_width: 24,
            registers: Vec::new(),
        },
        CpuInfo {
            id: 1,
            name: "Sony SPC700".to_string(),
            architecture: CpuArchitecture::Spc700,
            clock_speed: 1_024_000,
            location: CpuLocation::Motherboard,
            address_width: 16,
            registers: Vec::new(),
        },
    ]
}

/// CPU-roster entry (slot 2) for a cartridge's steppable secondary CPU, keyed
/// off the installed preset. The SA-1 contributes its second 65816
/// (`Mos65816`), the Super FX the GSU (`SuperFx`) and the Cx4 the HG51B
/// (`Hg51b`). Each reports its own clock: about 10.74 MHz for the SA-1 and
/// for the GSU at its default CLSR divider, 20 MHz for the Cx4. Every entry
/// gets the 24-bit-address debugger treatment, but the architecture tag
/// drives which register / flag layout the panel renders.
pub(super) fn secondary_cpu_info(preset: Option<PresetKind>) -> CpuInfo {
    let (name, architecture, clock_speed) = match preset {
        Some(PresetKind::SuperFx) => ("Super FX GSU", CpuArchitecture::SuperFx, 10_740_000),
        Some(PresetKind::Cx4) => ("Cx4 HG51B", CpuArchitecture::Hg51b, 20_000_000),
        _ => ("SA-1 (WDC 65C816)", CpuArchitecture::Mos65816, 10_740_000),
    };
    CpuInfo {
        id: 2,
        name: name.to_string(),
        architecture,
        clock_speed,
        location: CpuLocation::Cartridge,
        address_width: 24,
        registers: Vec::new(),
    }
}

/// Map a SNES destination byte (`$FFD9`) to a human-readable region
/// string used by [`RomInfo::region`]. The table is intentionally
/// coarse, "USA" covers both "USA" and "Canada". Unknown values
/// render as `"Unknown"`.
pub fn destination_region_str(byte: u8) -> &'static str {
    match byte {
        0x00 => "Japan",
        0x01 | 0x0F => "USA",
        0x02 => "Europe",
        0x03 => "Sweden",
        0x04 => "Finland",
        0x05 => "Denmark",
        0x06 => "France",
        0x07 => "Netherlands",
        0x08 => "Spain",
        0x09 => "Germany",
        0x0A => "Italy",
        0x0B => "China",
        0x0C => "Indonesia",
        0x0D => "Korea",
        0x10 => "Brazil",
        0x11 => "Australia",
        _ => "Unknown",
    }
}

/// Standard-pad button layout advertised via [`InputInfo`]. The `id`
/// of each entry is the generic `InputState::buttons` bit index the
/// host must set; it matches the `BTN_*` constants the controller
/// subsystem maps to the SNES hardware order.
pub(super) fn snes_button_layout() -> Vec<ButtonInfo> {
    use crate::controller::{
        BTN_A, BTN_B, BTN_DOWN, BTN_L, BTN_LEFT, BTN_R, BTN_RIGHT, BTN_SELECT, BTN_START, BTN_UP,
        BTN_X, BTN_Y,
    };
    let entry = |name: &str, id: u32| ButtonInfo {
        name: name.to_string(),
        id,
    };
    vec![
        entry("Up", BTN_UP),
        entry("Down", BTN_DOWN),
        entry("Left", BTN_LEFT),
        entry("Right", BTN_RIGHT),
        entry("A", BTN_A),
        entry("B", BTN_B),
        entry("X", BTN_X),
        entry("Y", BTN_Y),
        entry("L", BTN_L),
        entry("R", BTN_R),
        entry("Start", BTN_START),
        entry("Select", BTN_SELECT),
    ]
}

/// Stable name for a preset (used as the `mapper` field of
/// [`RomInfo`]).
pub(super) fn preset_kind_name(kind: PresetKind) -> String {
    match kind {
        PresetKind::LoRom => "LoROM".to_string(),
        PresetKind::HiRom => "HiROM".to_string(),
        PresetKind::ExHiRom => "ExHiROM".to_string(),
        PresetKind::Sdd1 => "S-DD1".to_string(),
        PresetKind::Spc7110 => "SPC7110".to_string(),
        PresetKind::Sa1 => "SA-1".to_string(),
        PresetKind::SuperFx => "Super FX".to_string(),
        PresetKind::Cx4 => "Cx4".to_string(),
        PresetKind::Custom => "Custom".to_string(),
    }
}
