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

/// The pad's buttons, as the host is told them.
pub(super) fn genesis_button_layout() -> Vec<ButtonInfo> {
    vec![
        ButtonInfo {
            name: "Up".to_string(),
            id: 0,
        },
        ButtonInfo {
            name: "Down".to_string(),
            id: 1,
        },
        ButtonInfo {
            name: "Left".to_string(),
            id: 2,
        },
        ButtonInfo {
            name: "Right".to_string(),
            id: 3,
        },
        ButtonInfo {
            name: "A".to_string(),
            id: 4,
        },
        ButtonInfo {
            name: "B".to_string(),
            id: 5,
        },
        ButtonInfo {
            name: "C".to_string(),
            id: 6,
        },
        ButtonInfo {
            name: "Start".to_string(),
            id: 7,
        },
        ButtonInfo {
            name: "X".to_string(),
            id: 8,
        },
        ButtonInfo {
            name: "Y".to_string(),
            id: 9,
        },
        ButtonInfo {
            name: "Z".to_string(),
            id: 10,
        },
        ButtonInfo {
            name: "Mode".to_string(),
            id: 11,
        },
    ]
}
