//! I/O Mini ROM Tests
//!
//! Tests real I/O communication using 68000 mini ROMs.
//! These tests validate:
//! - I/O register reads/writes
//! - Version register
//! - Controller protocol (3-button and 6-button)
//! - TH toggle detection
//! - Multi-port support

mod helpers;

use helpers::M68kRomBuilder;
use retro_core::cpu::Addressable;
use retro_core::input::{InputDevice, InputProvider};
use retro_core::EmulatorCore;
use sg1_core::io::devices::{Genesis3ButtonDevice, Genesis6ButtonDevice};
use sg1_core::system::GenesisEmulator;

// ============================================================================
// Test 1: Read Version Register
// ============================================================================

#[test]
fn test_minirom_read_version_register() {
    // ROM:
    // - Read version register (0xA10001)
    // - Store result in 0xFF0000 (RAM)
    // - Loop forever

    let rom = M68kRomBuilder::new(0x400)
        .move_b_abs_to_d0(0xA10001) // MOVE.B $A10001, D0 (read version)
        .move_b_d0_to_abs(0xFF0000) // MOVE.B D0, $FF0000 (store byte in RAM)
        .stop() // BRA.S -2: loop forever
        .build();

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("Failed to load ROM");

    // Run 100 steps (a step is one instruction): the two moves, then the
    // endless loop
    for _ in 0..100 {
        emu.step();
    }

    // Check result in RAM. The version register returns 0xA0 = Overseas
    // (bit 7) + NTSC (bit 6 = 0) + "expansion port disconnected"
    // (bit 5). See `read_version_register` in `io/mod.rs`.
    let result = emu.bus_mut().read(0xFF0000);
    assert_eq!(
        result, 0xA0,
        "Version register should return 0xA0 (Overseas NTSC, no expansion)"
    );
}

// ============================================================================
// Test 2: Data Register Loopback
// ============================================================================

#[test]
fn test_minirom_io_data_register_loopback() {
    // ROM:
    // - Set control register to all outputs (0xFF)
    // - Write 0x55 to data register
    // - Read data register
    // - Store result
    // - Loop forever

    let rom = M68kRomBuilder::new(0x400)
        .move_b_imm_to_abs(0xFF, 0xA10009) // Control = all outputs
        .delay(3) // Delay
        .move_b_imm_to_abs(0x55, 0xA10003) // Data = 0x55
        .delay(3) // Delay
        .move_b_abs_to_d0(0xA10003) // Read back
        .move_b_d0_to_abs(0xFF0000) // Store byte
        .stop()
        .build();

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("Failed to load ROM");

    // Run 200 steps; the ROM ends in an endless loop, so extra steps leave the result alone
    for _ in 0..200 {
        emu.step();
    }

    let result = emu.bus_mut().read(0xFF0000);
    assert_eq!(result, 0x55, "Should read back what was written");
}

// ============================================================================
// Test 3: 3-Button Controller - TH High
// ============================================================================

#[test]
fn test_minirom_3button_controller_th_high() {
    // ROM:
    // - Set control register (TH as output)
    // - Write TH=1 to data register
    // - Read data register
    // - Store result
    // - Loop forever

    let rom = M68kRomBuilder::new(0x400)
        .move_b_imm_to_abs(0x40, 0xA10009) // Control: TH output (bit 6)
        .delay(3)
        .move_b_imm_to_abs(0x40, 0xA10003) // Data: TH=1 (bit 6)
        .delay(3)
        .move_b_abs_to_d0(0xA10003) // Read with TH=1
        .move_b_d0_to_abs(0xFF0000) // Store byte
        .stop()
        .build();

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("Failed to load ROM");

    // Setup controller with UP and B pressed
    let mut device = Genesis3ButtonDevice::new();

    // Create a simple input provider for testing
    struct TestInputProvider(u32);
    impl InputProvider for TestInputProvider {
        fn get_state(&self) -> u32 {
            self.0
        }
        fn is_pressed(&self, button: u32) -> bool {
            (self.0 & (1 << button)) != 0
        }
        fn get_analog(&self, _axis: retro_core::input::AnalogAxis) -> f32 {
            0.0
        }
        fn provider_info(&self) -> retro_core::input::InputProviderInfo {
            retro_core::input::InputProviderInfo {
                name: "Test".to_string(),
                provider_type: retro_core::input::InputProviderType::Keyboard,
                id: "test".to_string(),
            }
        }
    }

    let provider =
        TestInputProvider((1 << Genesis3ButtonDevice::BTN_UP) | (1 << Genesis3ButtonDevice::BTN_B));
    device.update(&provider);

    // Connect device to port 0
    if let Some(port) = emu.bus_mut().io_mut().port_mut(0) {
        port.connect_device(Box::new(device));
    }

    // Run ROM (reduced steps)
    for _ in 0..150 {
        emu.step();
    }

    // Check result
    let th_high = emu.bus_mut().read(0xFF0000);

    // When TH=1: C B R L D U
    // UP pressed -> bit 0 should be 0
    // B pressed -> bit 4 should be 0
    assert_eq!(th_high & 0x01, 0x00, "UP pressed: bit 0 should be 0");
    assert_eq!(th_high & 0x10, 0x00, "B pressed: bit 4 should be 0");
}

// ============================================================================
// Test 4: 3-Button Controller - TH Low
// ============================================================================

#[test]
fn test_minirom_3button_controller_th_low() {
    // ROM:
    // - Set control register (TH as output)
    // - Write TH=0 to data register
    // - Read data register
    // - Store result
    // - Loop forever

    let rom = M68kRomBuilder::new(0x400)
        .move_b_imm_to_abs(0x40, 0xA10009) // Control: TH output (bit 6)
        .delay(3)
        .move_b_imm_to_abs(0x00, 0xA10003) // Data: TH=0
        .delay(3)
        .move_b_abs_to_d0(0xA10003) // Read with TH=0
        .move_b_d0_to_abs(0xFF0000) // Store byte
        .stop()
        .build();

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("Failed to load ROM");

    // Setup controller with A and START pressed
    struct TestInputProvider(u32);
    impl InputProvider for TestInputProvider {
        fn get_state(&self) -> u32 {
            self.0
        }
        fn is_pressed(&self, button: u32) -> bool {
            (self.0 & (1 << button)) != 0
        }
        fn get_analog(&self, _axis: retro_core::input::AnalogAxis) -> f32 {
            0.0
        }
        fn provider_info(&self) -> retro_core::input::InputProviderInfo {
            retro_core::input::InputProviderInfo {
                name: "Test".to_string(),
                provider_type: retro_core::input::InputProviderType::Keyboard,
                id: "test".to_string(),
            }
        }
    }

    let mut device = Genesis3ButtonDevice::new();
    let provider = TestInputProvider(
        (1 << Genesis3ButtonDevice::BTN_A) | (1 << Genesis3ButtonDevice::BTN_START),
    );
    device.update(&provider);

    if let Some(port) = emu.bus_mut().io_mut().port_mut(0) {
        port.connect_device(Box::new(device));
    }

    // Run ROM (reduced steps)
    for _ in 0..150 {
        emu.step();
    }

    // Check result
    let th_low = emu.bus_mut().read(0xFF0000);

    // When TH=0: S A 0 0 D U
    // A pressed -> bit 4 should be 0
    // START pressed -> bit 5 should be 0
    assert_eq!(th_low & 0x10, 0x00, "A pressed: bit 4 should be 0");
    assert_eq!(th_low & 0x20, 0x00, "START pressed: bit 5 should be 0");
}

// ============================================================================
// Test 5: 3-Button Controller - TH Toggle
// ============================================================================

#[test]
fn test_minirom_3button_controller_th_toggle() {
    // ROM:
    // - Set control register (TH as output)
    // - Write TH=1, read
    // - Write TH=0, read
    // - Store both results
    // - Loop forever

    let rom = M68kRomBuilder::new(0x400)
        .move_b_imm_to_abs(0x40, 0xA10009) // Control: TH output
        .delay(3)
        // Read with TH=1
        .move_b_imm_to_abs(0x40, 0xA10003) // Data: TH=1
        .delay(3)
        .move_b_abs_to_d0(0xA10003)
        .move_b_d0_to_abs(0xFF0000) // Store byte TH=1 result
        .delay(3)
        // Read with TH=0
        .move_b_imm_to_abs(0x00, 0xA10003) // Data: TH=0
        .delay(3)
        .move_b_abs_to_d0(0xA10003)
        .move_b_d0_to_abs(0xFF0004) // Store byte TH=0 result
        .stop()
        .build();

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("Failed to load ROM");

    // Setup controller with UP pressed
    struct TestInputProvider(u32);
    impl InputProvider for TestInputProvider {
        fn get_state(&self) -> u32 {
            self.0
        }
        fn is_pressed(&self, button: u32) -> bool {
            (self.0 & (1 << button)) != 0
        }
        fn get_analog(&self, _axis: retro_core::input::AnalogAxis) -> f32 {
            0.0
        }
        fn provider_info(&self) -> retro_core::input::InputProviderInfo {
            retro_core::input::InputProviderInfo {
                name: "Test".to_string(),
                provider_type: retro_core::input::InputProviderType::Keyboard,
                id: "test".to_string(),
            }
        }
    }

    let mut device = Genesis3ButtonDevice::new();
    let provider = TestInputProvider(1 << Genesis3ButtonDevice::BTN_UP);
    device.update(&provider);

    if let Some(port) = emu.bus_mut().io_mut().port_mut(0) {
        port.connect_device(Box::new(device));
    }

    // Run ROM (reduced steps)
    for _ in 0..200 {
        emu.step();
    }

    // Check results
    let th_high = emu.bus_mut().read(0xFF0000);
    let th_low = emu.bus_mut().read(0xFF0004);

    // UP pressed, so bit 0 should be 0 in both reads
    assert_eq!(th_high & 0x01, 0x00, "UP pressed: bit 0 should be 0 (TH=1)");
    assert_eq!(th_low & 0x01, 0x00, "UP pressed: bit 0 should be 0 (TH=0)");
}

// ============================================================================
// Test 6: 6-Button Controller - Cycle Sequence
// ============================================================================

#[test]
fn test_minirom_6button_controller_cycle_sequence() {
    // ROM:
    // - Set control register (TH as output)
    // - Toggle TH 6 times (3 full cycles)
    // - Read at cycle 3 TH=1 (should expose X/Y/Z/Mode)
    // - Store result
    // - Loop forever

    let mut builder = M68kRomBuilder::new(0x400);
    builder.move_b_imm_to_abs(0x40, 0xA10009); // Control: TH output
    builder.delay(3);

    // The pad counts TH's rising edges. Starting DATA.TH=0, the third
    // rise is the one after which TH=1 reads the extra buttons (CBMXYZ).
    // A fourth rise returns the pad to ordinary 3-button replies.
    for cycle in 0..3 {
        builder.move_b_imm_to_abs(0x40, 0xA10003);
        builder.delay(3);
        if cycle == 2 {
            builder.move_b_abs_to_d0(0xA10003);
            builder.move_b_d0_to_abs(0xFF0000);
        }
        builder.move_b_imm_to_abs(0x00, 0xA10003);
        builder.delay(3);
    }

    builder.stop();
    let rom = builder.build();

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("Failed to load ROM");

    // Setup 6-button controller with X and Y pressed
    struct TestInputProvider(u32);
    impl InputProvider for TestInputProvider {
        fn get_state(&self) -> u32 {
            self.0
        }
        fn is_pressed(&self, button: u32) -> bool {
            (self.0 & (1 << button)) != 0
        }
        fn get_analog(&self, _axis: retro_core::input::AnalogAxis) -> f32 {
            0.0
        }
        fn provider_info(&self) -> retro_core::input::InputProviderInfo {
            retro_core::input::InputProviderInfo {
                name: "Test".to_string(),
                provider_type: retro_core::input::InputProviderType::Keyboard,
                id: "test".to_string(),
            }
        }
    }

    let mut device = Genesis6ButtonDevice::new();
    let provider =
        TestInputProvider((1 << Genesis6ButtonDevice::BTN_X) | (1 << Genesis6ButtonDevice::BTN_Y));
    device.update(&provider);

    if let Some(port) = emu.bus_mut().io_mut().port_mut(0) {
        port.connect_device(Box::new(device));
    }

    // Run ROM (reduced steps)
    for _ in 0..300 {
        emu.step();
    }

    // Check step-7 result (X/Y/Z/Mode exposed as CBMXYZ)
    let cycle3_th1 = emu.bus_mut().read(0xFF0000);

    // TH=1 step 7: ?1CBMXYZ, X on D2, Y on D1
    assert_eq!(
        cycle3_th1 & 0x04,
        0x00,
        "X pressed: bit 2 should be 0 at step 7"
    );
    assert_eq!(
        cycle3_th1 & 0x02,
        0x00,
        "Y pressed: bit 1 should be 0 at step 7"
    );
}

// ============================================================================
// Test 7: Port 1 vs Port 2
// ============================================================================

#[test]
fn test_minirom_port1_vs_port2() {
    // ROM:
    // - Set control registers for both ports
    // - Write TH=1 to both ports
    // - Read from both ports
    // - Store results
    // - Loop forever

    let rom = M68kRomBuilder::new(0x400)
        // Configure Port 1
        .move_b_imm_to_abs(0x40, 0xA10009) // Control Port 1: TH output
        .move_b_imm_to_abs(0x40, 0xA10003) // Data Port 1: TH=1
        .delay(3)
        // Configure Port 2
        .move_b_imm_to_abs(0x40, 0xA1000B) // Control Port 2: TH output
        .move_b_imm_to_abs(0x40, 0xA10005) // Data Port 2: TH=1
        .delay(3)
        // Read Port 1
        .move_b_abs_to_d0(0xA10003)
        .move_b_d0_to_abs(0xFF0000)
        // Read Port 2
        .move_b_abs_to_d0(0xA10005)
        .move_b_d0_to_abs(0xFF0004)
        .stop()
        .build();

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("Failed to load ROM");

    // Setup Port 1 with UP pressed
    struct TestInputProvider(u32);
    impl InputProvider for TestInputProvider {
        fn get_state(&self) -> u32 {
            self.0
        }
        fn is_pressed(&self, button: u32) -> bool {
            (self.0 & (1 << button)) != 0
        }
        fn get_analog(&self, _axis: retro_core::input::AnalogAxis) -> f32 {
            0.0
        }
        fn provider_info(&self) -> retro_core::input::InputProviderInfo {
            retro_core::input::InputProviderInfo {
                name: "Test".to_string(),
                provider_type: retro_core::input::InputProviderType::Keyboard,
                id: "test".to_string(),
            }
        }
    }

    let mut device1 = Genesis3ButtonDevice::new();
    let provider1 = TestInputProvider(1 << Genesis3ButtonDevice::BTN_UP);
    device1.update(&provider1);

    if let Some(port) = emu.bus_mut().io_mut().port_mut(0) {
        port.connect_device(Box::new(device1));
    }

    // Setup Port 2 with DOWN pressed
    let mut device2 = Genesis3ButtonDevice::new();
    let provider2 = TestInputProvider(1 << Genesis3ButtonDevice::BTN_DOWN);
    device2.update(&provider2);

    if let Some(port) = emu.bus_mut().io_mut().port_mut(1) {
        port.connect_device(Box::new(device2));
    }

    // Run ROM (it ends in an endless loop, so extra steps leave the results alone)
    for _ in 0..300 {
        emu.step();
    }

    // Check results
    let port1_data = emu.bus_mut().read(0xFF0000);
    let port2_data = emu.bus_mut().read(0xFF0004);

    // Port 1: UP pressed (bit 0 = 0)
    assert_eq!(
        port1_data & 0x01,
        0x00,
        "Port 1 UP pressed: bit 0 should be 0"
    );

    // Port 2: DOWN pressed (bit 1 = 0)
    assert_eq!(
        port2_data & 0x02,
        0x00,
        "Port 2 DOWN pressed: bit 1 should be 0"
    );

    // Different ports should have different values
    assert_ne!(
        port1_data, port2_data,
        "Port 1 and Port 2 should have different values"
    );
}
