//! Genesis controller port
//!
//! Implements the controller port logic for Genesis,
//! including TH toggle tracking for 6-button controllers.

use retro_core::input::InputDevice;

/// Genesis controller port
///
/// Manages a single controller port and tracks TH toggles
/// for 6-button protocol
pub struct GenesisControllerPort {
    port_id: u32,
    device: Option<Box<dyn InputDevice>>,
    th_toggle_count: u32,
}

impl GenesisControllerPort {
    pub fn new(port_id: u32) -> Self {
        Self {
            port_id,
            device: None,
            th_toggle_count: 0,
        }
    }

    /// Read from port
    ///
    /// Returns device state based on TH value
    pub fn read(&self, th: bool) -> u8 {
        if let Some(device) = &self.device {
            device.read_state(if th { 1 } else { 0 })
        } else {
            0xFF // No device connected (pulled high)
        }
    }

    /// Handle a TH 0->1 rising edge (6-button phase advance).
    ///
    /// The pad counter steps by +2 per rise,
    /// capped at 8. The port mirror is what savestates capture.
    pub fn on_th_rising(&mut self) {
        if let Some(device) = &mut self.device {
            device.clock(0);
        }
        if self.th_toggle_count < 8 {
            // Keep the port mirror on the same +2 cadence as Genesis6ButtonDevice.
            // 3-button devices ignore clock(); the unused mirror is harmless.
            self.th_toggle_count += 2;
        }
    }

    /// Reset timeout
    ///
    /// Called after 1.5ms without TH toggle
    pub fn reset_timeout(&mut self) {
        self.th_toggle_count = 0;
        if let Some(device) = &mut self.device {
            device.reset();
        }
    }

    /// Connect a device to this port
    pub fn connect_device(&mut self, device: Box<dyn InputDevice>) {
        self.device = Some(device);
    }

    /// Disconnect device
    pub fn disconnect_device(&mut self) {
        self.device = None;
    }

    /// Get reference to connected device
    pub fn get_device(&self) -> Option<&dyn InputDevice> {
        self.device.as_ref().map(|d| d.as_ref())
    }

    /// Get mutable reference to connected device
    pub fn get_device_mut(&mut self) -> Option<&mut Box<dyn InputDevice>> {
        self.device.as_mut()
    }

    /// Set the connected device's button state directly from a bitmask.
    /// This is used by the frame loop to sync InputManager state into the I/O devices.
    pub fn set_device_button_state(&mut self, state: u32) {
        if let Some(device) = &mut self.device {
            device.set_state_direct(state);
        }
    }

    /// Get port ID
    pub fn port_id(&self) -> u32 {
        self.port_id
    }

    /// Get current TH toggle count
    pub fn th_toggle_count(&self) -> u32 {
        self.th_toggle_count
    }

    /// Restore the TH pulse counter from a savestate. The count is part of
    /// the 6-button polling protocol phase and is captured by
    /// `GenesisIo::to_savestate`, so it must be reinstated on load for a
    /// bit-exact round-trip (netplay rollback / desync detection).
    pub fn set_th_toggle_count(&mut self, count: u32) {
        self.th_toggle_count = count;
        if let Some(device) = &mut self.device {
            // Replay rises so the device's private counter matches the port.
            device.reset();
            for _ in 0..(count / 2) {
                device.clock(0);
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::input::{DeviceInfo, DeviceType, InputProvider};

    // Mock device for testing
    struct MockDevice {
        state: u8,
    }

    impl InputDevice for MockDevice {
        fn device_type(&self) -> DeviceType {
            DeviceType::Genesis3Button
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo {
                name: "Mock Device".to_string(),
                device_type: DeviceType::Genesis3Button,
                button_count: 8,
                has_analog: false,
            }
        }

        fn update(&mut self, _provider: &dyn InputProvider) {}

        fn read_state(&self, _control_state: u8) -> u8 {
            self.state
        }

        fn reset(&mut self) {}

        fn clock(&mut self, _cycles: u32) {}
    }

    #[test]
    fn test_port_creation() {
        let port = GenesisControllerPort::new(0);
        assert_eq!(port.port_id(), 0);
        assert_eq!(port.th_toggle_count(), 0);
        assert!(port.get_device().is_none());
    }

    #[test]
    fn test_read_no_device() {
        let port = GenesisControllerPort::new(0);
        // No device connected
        assert_eq!(port.read(true), 0xFF);
        assert_eq!(port.read(false), 0xFF);
    }

    #[test]
    fn test_connect_device() {
        let mut port = GenesisControllerPort::new(0);
        let device = Box::new(MockDevice { state: 0xAB });
        port.connect_device(device);

        assert!(port.get_device().is_some());
        assert_eq!(port.read(true), 0xAB);
    }

    #[test]
    fn test_disconnect_device() {
        let mut port = GenesisControllerPort::new(0);
        let device = Box::new(MockDevice { state: 0xAB });
        port.connect_device(device);

        port.disconnect_device();
        assert!(port.get_device().is_none());
        assert_eq!(port.read(true), 0xFF);
    }

    #[test]
    fn test_th_rising_advances_by_two() {
        let mut port = GenesisControllerPort::new(0);
        assert_eq!(port.th_toggle_count(), 0);

        port.on_th_rising();
        assert_eq!(port.th_toggle_count(), 2);

        port.on_th_rising();
        assert_eq!(port.th_toggle_count(), 4);
    }

    #[test]
    fn test_reset_timeout() {
        let mut port = GenesisControllerPort::new(0);
        port.on_th_rising();
        port.on_th_rising();
        assert_eq!(port.th_toggle_count(), 4);

        port.reset_timeout();
        assert_eq!(port.th_toggle_count(), 0);
    }
}
