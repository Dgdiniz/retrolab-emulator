//! Tests for the generic retro-core API.
//!
//! These tests validate that the trait definitions are sound and can be used
//! to build emulator cores.

use retro_core::*;

#[test]
fn test_system_info_serialization() {
    let sys_info = SystemInfo {
        name: "Test Console".to_string(),
        short_name: "TEST".to_string(),
        manufacturer: "Test Corp".to_string(),
        year: 1991,
        cpus: vec![CpuInfo {
            id: 0,
            name: "TestCPU".to_string(),
            architecture: CpuArchitecture::M68000,
            clock_speed: 7_670_454,
            registers: vec![RegisterInfo {
                name: "D0".to_string(),
                size_bits: 32,
                display_format: DisplayFormat::Hexadecimal,
                category: RegisterCategory::General,
            }],
            location: CpuLocation::Motherboard,
            address_width: 24,
        }],
        display: DisplayInfo {
            width: 320,
            height: 224,
            aspect_ratio: 4.0 / 3.0,
            refresh_rate: 60.0,
            pixel_format: PixelFormat::RGBA8888,
        },
        audio: AudioInfo {
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
        },
        input: InputInfo {
            max_players: 2,
            button_layout: vec![
                ButtonInfo {
                    name: "A".to_string(),
                    id: 0,
                },
                ButtonInfo {
                    name: "B".to_string(),
                    id: 1,
                },
            ],
        },
    };

    // Test serialization
    let json = serde_json::to_string(&sys_info).unwrap();
    assert!(json.contains("Test Console"));

    // Test deserialization
    let deserialized: SystemInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.name, "Test Console");
    assert_eq!(deserialized.cpus.len(), 1);
    assert_eq!(deserialized.cpus[0].architecture, CpuArchitecture::M68000);
}

#[test]
fn test_framebuffer_creation() {
    let fb = Framebuffer::new(320, 224);
    assert_eq!(fb.width, 320);
    assert_eq!(fb.height, 224);
    assert_eq!(fb.data.len(), 320 * 224 * 4); // RGBA
}

#[test]
fn test_breakpoint_serialization() {
    let bp = Breakpoint {
        address: 0x400,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };

    let json = serde_json::to_string(&bp).unwrap();
    let deserialized: Breakpoint = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.address, 0x400);
    assert_eq!(deserialized.bp_type, BreakpointType::execute());
    assert_eq!(deserialized.cpu_id, Some(0));
}

#[test]
fn test_memory_regions() {
    let regions = vec![
        MemoryRegion::MainRam,
        MemoryRegion::VideoRam,
        MemoryRegion::Rom,
        MemoryRegion::Custom(0),
    ];

    for region in regions {
        let json = serde_json::to_string(&region).unwrap();
        let _deserialized: MemoryRegion = serde_json::from_str(&json).unwrap();
    }
}

#[test]
fn test_execution_modes() {
    let modes = vec![
        ExecutionMode::Running,
        ExecutionMode::Paused,
        ExecutionMode::StepInto,
        ExecutionMode::StepOver,
        ExecutionMode::StepOut,
    ];

    for mode in modes {
        let json = serde_json::to_string(&mode).unwrap();
        let _deserialized: ExecutionMode = serde_json::from_str(&json).unwrap();
    }
}
