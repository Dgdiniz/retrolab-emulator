//! Genesis 3-button controller
//!
//! Implements the standard Genesis 3-button controller with:
//! - D-Pad: Up, Down, Left, Right
//! - Face buttons: A, B, C
//! - Start button
//!
//! # Protocol
//! TH=1: C B R L D U (bits 5-0)
//! TH=0: S A 0 0 D U (bits 5-0)

use retro_core::input::{DeviceInfo, DeviceType, InputDevice, InputProvider};

/// Genesis 3-button controller
pub struct Genesis3ButtonDevice {
    /// Button state bitmask
    state: u32,
}

impl Genesis3ButtonDevice {
    pub fn new() -> Self {
        Self { state: 0 }
    }

    // Button indices (bit positions)
    pub const BTN_UP: u32 = 0;
    pub const BTN_DOWN: u32 = 1;
    pub const BTN_LEFT: u32 = 2;
    pub const BTN_RIGHT: u32 = 3;
    pub const BTN_B: u32 = 4;
    pub const BTN_C: u32 = 5;
    pub const BTN_A: u32 = 6;
    pub const BTN_START: u32 = 7;

    /// Get button state
    pub fn get_state(&self) -> u32 {
        self.state
    }

    /// Set button state
    pub fn set_state(&mut self, state: u32) {
        self.state = state;
    }
}

impl Default for Genesis3ButtonDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl InputDevice for Genesis3ButtonDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Genesis3Button
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            name: "Genesis 3-Button Controller".to_string(),
            device_type: DeviceType::Genesis3Button,
            button_count: 8,
            has_analog: false,
        }
    }

    fn update(&mut self, provider: &dyn InputProvider) {
        self.state = provider.get_state();
    }

    fn read_state(&self, control_state: u8) -> u8 {
        let th = (control_state & 0x01) != 0;

        if th {
            // TH=1: C B R L D U
            let mut data = 0xFF;
            if self.state & (1 << Self::BTN_UP) != 0 {
                data &= !0x01;
            }
            if self.state & (1 << Self::BTN_DOWN) != 0 {
                data &= !0x02;
            }
            if self.state & (1 << Self::BTN_LEFT) != 0 {
                data &= !0x04;
            }
            if self.state & (1 << Self::BTN_RIGHT) != 0 {
                data &= !0x08;
            }
            if self.state & (1 << Self::BTN_B) != 0 {
                data &= !0x10;
            }
            if self.state & (1 << Self::BTN_C) != 0 {
                data &= !0x20;
            }
            data
        } else {
            // TH=0: S A 0 0 D U
            let mut data = 0xFF;
            if self.state & (1 << Self::BTN_UP) != 0 {
                data &= !0x01;
            }
            if self.state & (1 << Self::BTN_DOWN) != 0 {
                data &= !0x02;
            }
            // Bits 2-3 are always 0
            data &= !0x0C;
            if self.state & (1 << Self::BTN_A) != 0 {
                data &= !0x10;
            }
            if self.state & (1 << Self::BTN_START) != 0 {
                data &= !0x20;
            }
            data
        }
    }

    fn reset(&mut self) {
        // No reset needed for 3-button
    }

    fn clock(&mut self, _cycles: u32) {
        // No timing needed
    }

    fn set_state_direct(&mut self, state: u32) {
        self.state = state;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_creation() {
        let device = Genesis3ButtonDevice::new();
        assert_eq!(device.get_state(), 0);
        assert_eq!(device.device_type(), DeviceType::Genesis3Button);
    }

    #[test]
    fn test_device_info() {
        let device = Genesis3ButtonDevice::new();
        let info = device.device_info();
        assert_eq!(info.device_type, DeviceType::Genesis3Button);
        assert_eq!(info.button_count, 8);
        assert!(!info.has_analog);
    }

    #[test]
    fn test_read_state_th_high_no_buttons() {
        let device = Genesis3ButtonDevice::new();
        // TH=1, no buttons pressed
        let data = device.read_state(1);
        // All bits high except what's explicitly set
        assert_eq!(data, 0xFF);
    }

    #[test]
    fn test_read_state_th_high_up_pressed() {
        let mut device = Genesis3ButtonDevice::new();
        device.set_state(1 << Genesis3ButtonDevice::BTN_UP);

        // TH=1
        let data = device.read_state(1);
        // UP pressed, so bit 0 should be 0
        assert_eq!(data & 0x01, 0x00);
        // Other bits should be 1
        assert_eq!(data & 0xFE, 0xFE);
    }

    #[test]
    fn test_read_state_th_high_b_pressed() {
        let mut device = Genesis3ButtonDevice::new();
        device.set_state(1 << Genesis3ButtonDevice::BTN_B);

        // TH=1
        let data = device.read_state(1);
        // B pressed, so bit 4 should be 0
        assert_eq!(data & 0x10, 0x00);
    }

    #[test]
    fn test_read_state_th_low_no_buttons() {
        let device = Genesis3ButtonDevice::new();
        // TH=0, no buttons pressed
        let data = device.read_state(0);
        // Bits 2-3 always 0, others high
        assert_eq!(data & 0x0C, 0x00);
        assert_eq!(data & 0xF3, 0xF3);
    }

    #[test]
    fn test_read_state_th_low_a_pressed() {
        let mut device = Genesis3ButtonDevice::new();
        device.set_state(1 << Genesis3ButtonDevice::BTN_A);

        // TH=0
        let data = device.read_state(0);
        // A pressed, so bit 4 should be 0
        assert_eq!(data & 0x10, 0x00);
        // Bits 2-3 still 0
        assert_eq!(data & 0x0C, 0x00);
    }

    #[test]
    fn test_read_state_th_low_start_pressed() {
        let mut device = Genesis3ButtonDevice::new();
        device.set_state(1 << Genesis3ButtonDevice::BTN_START);

        // TH=0
        let data = device.read_state(0);
        // START pressed, so bit 5 should be 0
        assert_eq!(data & 0x20, 0x00);
    }

    #[test]
    fn test_read_state_multiple_buttons() {
        let mut device = Genesis3ButtonDevice::new();
        device.set_state(
            (1 << Genesis3ButtonDevice::BTN_UP)
                | (1 << Genesis3ButtonDevice::BTN_B)
                | (1 << Genesis3ButtonDevice::BTN_A),
        );

        // TH=1: UP and B should be visible
        let data_th1 = device.read_state(1);
        assert_eq!(data_th1 & 0x01, 0x00); // UP
        assert_eq!(data_th1 & 0x10, 0x00); // B

        // TH=0: UP and A should be visible
        let data_th0 = device.read_state(0);
        assert_eq!(data_th0 & 0x01, 0x00); // UP
        assert_eq!(data_th0 & 0x10, 0x00); // A
    }
}
