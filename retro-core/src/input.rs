//! Generic input system for multi-console emulation
//!
//! This module defines traits and types for handling input across different
//! console systems (Genesis, SNES, NES, etc.). The architecture separates:
//! - Input providers (keyboard, gamepad, etc.)
//! - Input devices (3-button controller, 6-button controller, etc.)
//! - Controller ports (console-specific protocols)
//! - Input debugging (state inspection, history, etc.)

pub mod manager;
pub mod providers;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

// Re-export providers for convenience
pub use manager::GenericInputManager;
pub use providers::{GamepadInputProvider, KeyboardInputProvider};

// ============================================================================
// Input Provider Traits
// ============================================================================

/// Generic input provider trait
///
/// Implementations: KeyboardInputProvider, GamepadInputProvider.
/// Provides a unified interface for different input sources.
pub trait InputProvider {
    /// Get current state as a bitmask
    /// Each console defines its own button mapping
    fn get_state(&self) -> u32;

    /// Check if a specific button is pressed
    fn is_pressed(&self, button: u32) -> bool {
        (self.get_state() & (1 << button)) != 0
    }

    /// Get analog value (for analog sticks, -1.0 to 1.0)
    fn get_analog(&self, axis: AnalogAxis) -> f32;

    /// Get provider info (keyboard, gamepad, etc.)
    fn provider_info(&self) -> InputProviderInfo;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputProviderInfo {
    pub name: String,
    pub provider_type: InputProviderType,
    pub id: String, // Device ID (e.g., "keyboard", "gamepad-0")
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputProviderType {
    Keyboard,
    Gamepad,
    Touch,
    Mouse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AnalogAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    TriggerLeft,
    TriggerRight,
}

// ============================================================================
// Input Mapper Trait
// ============================================================================

/// Maps generic input to console-specific buttons
pub trait InputMapper {
    /// Map provider state to console button state
    fn map(&self, provider_state: u32) -> u32;

    /// Get mapping configuration
    fn get_mapping(&self) -> &InputMapping;

    /// Set mapping configuration
    fn set_mapping(&mut self, mapping: InputMapping);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMapping {
    /// Map provider button index to console button index
    pub button_map: HashMap<u32, u32>,

    /// Map analog axis to d-pad (optional)
    pub analog_to_dpad: Option<AnalogToDPad>,

    /// Deadzone for analog sticks (0.0 to 1.0)
    pub deadzone: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalogToDPad {
    pub axis_x: AnalogAxis,
    pub axis_y: AnalogAxis,
    pub threshold: f32, // 0.0 to 1.0
}

// ============================================================================
// Controller Port Trait
// ============================================================================

/// Generic controller port trait
///
/// Abstracts console-specific communication protocols:
/// - Genesis: TH/TR/TL multiplexing
/// - SNES: Clock/Latch serial
/// - NES: Strobe/Shift serial
pub trait ControllerPort {
    /// Read from port (console-specific protocol)
    fn read(&mut self) -> u8;

    /// Write to port (e.g., TH toggle, strobe, etc.)
    fn write(&mut self, value: u8);

    /// Get port info (type, connected device, etc.)
    fn port_info(&self) -> PortInfo;

    /// Connect a device to this port
    fn connect_device(&mut self, device: Box<dyn InputDevice>);

    /// Disconnect device
    fn disconnect_device(&mut self);

    /// Clock the port (for timing-sensitive protocols)
    fn clock(&mut self, cycles: u32);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortInfo {
    pub port_id: u32,
    pub port_type: PortType,
    pub connected_device: Option<DeviceInfo>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PortType {
    Genesis3Pin,
    Snes5Pin,
    Nes7Pin,
    // Future: PlayStation, N64, etc.
}

// ============================================================================
// Input Device Trait
// ============================================================================

/// Generic input device (controller, mouse, light gun, etc.)
///
/// Represents a physical device connected to a console port.
/// Handles protocol-specific state reading.
pub trait InputDevice: Send {
    /// Get device type
    fn device_type(&self) -> DeviceType;

    /// Get device info
    fn device_info(&self) -> DeviceInfo;

    /// Update device state from input provider
    fn update(&mut self, provider: &dyn InputProvider);

    /// Read device state (protocol-specific)
    /// - Genesis: depends on TH state (control_state)
    /// - SNES: shift register
    /// - NES: shift register
    fn read_state(&self, control_state: u8) -> u8;

    /// Reset device (e.g., strobe, timeout)
    fn reset(&mut self);

    /// Clock device (for timing)
    fn clock(&mut self, cycles: u32);

    /// Set the device button state directly from a bitmask.
    /// Used by the frame loop to sync input state without needing an InputProvider reference.
    /// Default implementation does nothing; override in concrete devices.
    fn set_state_direct(&mut self, _state: u32) {}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub device_type: DeviceType,
    pub button_count: u32,
    pub has_analog: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeviceType {
    // Genesis
    Genesis3Button,
    Genesis6Button,
    GenesisMouse,

    // SNES
    SnesStandardPad,
    SnesMouse,

    // NES
    NesStandardPad,
    NesZapper,

    // Generic
    None,
}

// ============================================================================
// Input Debugger Trait
// ============================================================================

/// Input debugging interface
///
/// Provides state inspection, history, and mapping for debugging
pub trait InputDebugger {
    /// Get state of all connected devices
    fn get_device_states(&self) -> Vec<DeviceState>;

    /// Get state of a specific port
    fn get_port_state(&self, port_id: u32) -> Option<PortState>;

    /// Get input history (last N inputs)
    fn get_input_history(&self, count: u32) -> Vec<InputSnapshot>;

    /// Get current input mapping
    fn get_input_mapping(&self) -> Vec<InputMapping>;

    /// Set input mapping
    fn set_input_mapping(&mut self, port_id: u32, mapping: InputMapping);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceState {
    pub device_info: DeviceInfo,
    pub port_id: u32,
    pub buttons: Vec<ButtonState>,
    pub analog: Vec<AnalogState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonState {
    pub name: String,
    pub index: u32,
    pub pressed: bool,
    pub frames_held: u32, // For debugging
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalogState {
    pub axis: AnalogAxis,
    pub value: f32, // -1.0 to 1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortState {
    pub port_id: u32,
    pub control_register: u8,
    pub data_register: u8,
    pub th_state: Option<bool>, // Genesis-specific
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub frame: u64,
    pub timestamp_ms: u64,
    pub devices: Vec<DeviceState>,
}

// ============================================================================
// Gamepad button maps across a JavaScript boundary
// ============================================================================

/// A gamepad button map (button index -> console button) with its keys
/// written as text.
///
/// JavaScript holds every object key as a string, so a browser host sends
/// and reads a pad's map as `{ "0": 6, "12": 0, ... }`. Writing the keys as
/// text is what lets the map become a plain object at all; a serializer
/// asked to emit number keys as object keys refuses. Ordered, so the
/// object's keys come out in button order.
pub fn gamepad_map_with_text_keys(map: &HashMap<u32, u32>) -> BTreeMap<String, u32> {
    map.iter()
        .map(|(button, bit)| (button.to_string(), *bit))
        .collect()
}

/// Read a gamepad button map whose keys are text, as
/// [`gamepad_map_with_text_keys`] writes it. `None` if any key is not a
/// button index, so a malformed map is refused whole rather than read in
/// part.
pub fn gamepad_map_from_text_keys(map: HashMap<String, u32>) -> Option<HashMap<u32, u32>> {
    map.into_iter()
        .map(|(button, bit)| button.parse::<u32>().ok().map(|button| (button, bit)))
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamepad_map_text_keys_round_trip() {
        let mut map = HashMap::new();
        map.insert(12u32, 0u32);
        map.insert(0u32, 6u32);
        let text = gamepad_map_with_text_keys(&map);
        assert_eq!(
            text.keys().cloned().collect::<Vec<_>>(),
            vec!["0".to_string(), "12".to_string()]
        );
        assert_eq!(
            gamepad_map_from_text_keys(text.into_iter().collect()),
            Some(map)
        );
    }

    #[test]
    fn gamepad_map_from_text_keys_refuses_a_key_that_is_not_a_button_index() {
        let mut map = HashMap::new();
        map.insert("0".to_string(), 6u32);
        map.insert("up".to_string(), 0u32);
        assert_eq!(gamepad_map_from_text_keys(map), None);
        assert_eq!(
            gamepad_map_from_text_keys(HashMap::new()),
            Some(HashMap::new())
        );
    }

    #[test]
    fn test_input_provider_info() {
        let info = InputProviderInfo {
            name: "Test Keyboard".to_string(),
            provider_type: InputProviderType::Keyboard,
            id: "keyboard".to_string(),
        };
        assert_eq!(info.provider_type, InputProviderType::Keyboard);
        assert_eq!(info.id, "keyboard");
    }

    #[test]
    fn test_device_info() {
        let info = DeviceInfo {
            name: "Genesis 3-Button".to_string(),
            device_type: DeviceType::Genesis3Button,
            button_count: 8,
            has_analog: false,
        };
        assert_eq!(info.device_type, DeviceType::Genesis3Button);
        assert_eq!(info.button_count, 8);
        assert!(!info.has_analog);
    }

    #[test]
    fn test_port_info() {
        let info = PortInfo {
            port_id: 0,
            port_type: PortType::Genesis3Pin,
            connected_device: None,
        };
        assert_eq!(info.port_id, 0);
        assert_eq!(info.port_type, PortType::Genesis3Pin);
        assert!(info.connected_device.is_none());
    }

    #[test]
    fn test_button_state() {
        let state = ButtonState {
            name: "A".to_string(),
            index: 0,
            pressed: true,
            frames_held: 5,
        };
        assert_eq!(state.name, "A");
        assert!(state.pressed);
        assert_eq!(state.frames_held, 5);
    }
}
