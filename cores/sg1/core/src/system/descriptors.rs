//! What the host is told about the console: its CPUs with their
//! registers, and the pad's buttons.

use retro_core::*;

/// The 68000 and the Z80, with their registers, as the host is told them.
pub(super) fn genesis_cpus() -> Vec<CpuInfo> {
    vec![
        CpuInfo {
            id: 0,
            name: "Motorola 68000".to_string(),
            architecture: CpuArchitecture::M68000,
            clock_speed: 7_670_454, // ~7.67 MHz
            location: CpuLocation::Motherboard,
            address_width: 24,
            registers: vec![
                // Data registers
                RegisterInfo {
                    name: "D0".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "D1".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "D2".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "D3".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "D4".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "D5".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "D6".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "D7".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                // Address registers
                RegisterInfo {
                    name: "A0".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address,
                },
                RegisterInfo {
                    name: "A1".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address,
                },
                RegisterInfo {
                    name: "A2".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address,
                },
                RegisterInfo {
                    name: "A3".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address,
                },
                RegisterInfo {
                    name: "A4".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address,
                },
                RegisterInfo {
                    name: "A5".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address,
                },
                RegisterInfo {
                    name: "A6".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address,
                },
                RegisterInfo {
                    name: "A7".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address,
                },
                // Special registers
                RegisterInfo {
                    name: "PC".to_string(),
                    size_bits: 32,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Special,
                },
                RegisterInfo {
                    name: "SR".to_string(),
                    size_bits: 16,
                    display_format: DisplayFormat::Binary,
                    category: RegisterCategory::Flags,
                },
            ],
        },
        CpuInfo {
            id: 1,
            name: "Zilog Z80".to_string(),
            architecture: CpuArchitecture::Z80,
            clock_speed: 3_579_545, // ~3.58 MHz
            location: CpuLocation::Motherboard,
            address_width: 16,
            registers: vec![
                // Main register set
                RegisterInfo {
                    name: "A".to_string(),
                    size_bits: 8,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "F".to_string(),
                    size_bits: 8,
                    display_format: DisplayFormat::Binary,
                    category: RegisterCategory::Flags,
                },
                RegisterInfo {
                    name: "B".to_string(),
                    size_bits: 8,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "C".to_string(),
                    size_bits: 8,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "D".to_string(),
                    size_bits: 8,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "E".to_string(),
                    size_bits: 8,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "H".to_string(),
                    size_bits: 8,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                RegisterInfo {
                    name: "L".to_string(),
                    size_bits: 8,
                    display_format: DisplayFormat::Hexadecimal,
                    category: RegisterCategory::General,
                },
                // Special registers
                RegisterInfo {
                    name: "PC".to_string(),
                    size_bits: 16,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Special,
                },
                RegisterInfo {
                    name: "SP".to_string(),
                    size_bits: 16,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Special,
                },
                RegisterInfo {
                    name: "IX".to_string(),
                    size_bits: 16,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address, // Index register (address-like)
                },
                RegisterInfo {
                    name: "IY".to_string(),
                    size_bits: 16,
                    display_format: DisplayFormat::Address,
                    category: RegisterCategory::Address, // Index register (address-like)
                },
            ],
        },
    ]
}

/// The pad's buttons, as the host is told them: each id the button's bit
/// in `InputState::buttons`, the bit the six-button pad reads it from.
pub(super) fn genesis_button_layout() -> Vec<ButtonInfo> {
    use crate::io::devices::Genesis6ButtonDevice as Pad;
    [
        ("Up", Pad::BTN_UP),
        ("Down", Pad::BTN_DOWN),
        ("Left", Pad::BTN_LEFT),
        ("Right", Pad::BTN_RIGHT),
        ("A", Pad::BTN_A),
        ("B", Pad::BTN_B),
        ("C", Pad::BTN_C),
        ("Start", Pad::BTN_START),
        ("X", Pad::BTN_X),
        ("Y", Pad::BTN_Y),
        ("Z", Pad::BTN_Z),
        ("Mode", Pad::BTN_MODE),
    ]
    .into_iter()
    .map(|(name, id)| ButtonInfo {
        name: name.to_string(),
        id,
    })
    .collect()
}
