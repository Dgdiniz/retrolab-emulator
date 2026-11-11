//! Gamepad input provider
//!
//! Supports Web Gamepad API with analog stick support and deadzone.

use crate::input::{AnalogAxis, AnalogToDPad, InputProvider, InputProviderInfo, InputProviderType};
use std::collections::HashMap;

/// Gamepad input provider
///
/// Supports standard gamepads with:
/// - 16 buttons (Web Gamepad API standard)
/// - 4 axes (LeftX, LeftY, RightX, RightY)
/// - Analog to D-Pad conversion with threshold
/// - Configurable deadzone
pub struct GamepadInputProvider {
    /// Gamepad ID (e.g., "gamepad-0")
    gamepad_id: String,

    /// Button states (up to 16 standard buttons)
    button_state: [bool; 16],

    /// Axis states (4 axes: LeftX, LeftY, RightX, RightY)
    axis_state: [f32; 4],

    /// Button mapping (gamepad button index -> console button index)
    button_mapping: HashMap<u32, u32>,

    /// Analog to D-Pad configuration (optional)
    analog_to_dpad_config: Option<AnalogToDPad>,

    /// Deadzone for analog sticks (0.0 to 1.0)
    deadzone: f32,
}

impl GamepadInputProvider {
    /// Create a new gamepad provider with default Genesis mapping
    pub fn new(gamepad_id: String) -> Self {
        Self {
            gamepad_id,
            button_state: [false; 16],
            axis_state: [0.0; 4],
            button_mapping: Self::default_genesis_mapping(),
            analog_to_dpad_config: Some(AnalogToDPad {
                axis_x: AnalogAxis::LeftX,
                axis_y: AnalogAxis::LeftY,
                threshold: 0.5,
            }),
            deadzone: 0.15,
        }
    }

    /// Create a new gamepad provider with custom mapping
    pub fn with_mapping(gamepad_id: String, button_mapping: HashMap<u32, u32>) -> Self {
        Self {
            gamepad_id,
            button_state: [false; 16],
            axis_state: [0.0; 4],
            button_mapping,
            analog_to_dpad_config: Some(AnalogToDPad {
                axis_x: AnalogAxis::LeftX,
                axis_y: AnalogAxis::LeftY,
                threshold: 0.5,
            }),
            deadzone: 0.15,
        }
    }

    /// Default Genesis button mapping for standard gamepads
    ///
    /// Maps Web Gamepad API standard button indices to Genesis:
    /// - Button 0 (A/Cross) -> Genesis A (button 6)
    /// - Button 1 (B/Circle) -> Genesis B (button 4)
    /// - Button 2 (X/Square) -> Genesis C (button 5)
    /// - Button 3 (Y/Triangle) -> Genesis X (button 10)
    /// - Button 4 (L1/LB) -> Genesis Y (button 9)
    /// - Button 5 (R1/RB) -> Genesis Z (button 8)
    /// - Button 9 (Start) -> Start (button 7)
    /// - Button 8 (Select/Back) -> Mode (button 11)
    /// - Buttons 12-15 (D-Pad) -> D-Pad (buttons 0-3)
    fn default_genesis_mapping() -> HashMap<u32, u32> {
        let mut map = HashMap::new();

        // Face buttons
        map.insert(0, 6); // A/Cross -> Genesis A
        map.insert(1, 4); // B/Circle -> Genesis B
        map.insert(2, 5); // X/Square -> Genesis C
        map.insert(3, 10); // Y/Triangle -> Genesis X

        // Shoulder buttons
        map.insert(4, 9); // L1/LB -> Genesis Y
        map.insert(5, 8); // R1/RB -> Genesis Z

        // Start/Select
        map.insert(9, 7); // Start -> Start
        map.insert(8, 11); // Select/Back -> Mode

        // D-Pad (buttons 12-15)
        map.insert(12, 0); // D-Pad Up
        map.insert(13, 1); // D-Pad Down
        map.insert(14, 2); // D-Pad Left
        map.insert(15, 3); // D-Pad Right

        map
    }

    /// Update button state
    ///
    /// # Arguments
    /// * `button_index` - Web Gamepad API button index (0-15)
    /// * `pressed` - Whether the button is pressed
    pub fn update_button(&mut self, button_index: u32, pressed: bool) {
        if button_index < 16 {
            self.button_state[button_index as usize] = pressed;
        }
    }

    /// Update axis state
    ///
    /// # Arguments
    /// * `axis_index` - Axis index (0=LeftX, 1=LeftY, 2=RightX, 3=RightY)
    /// * `value` - Axis value (-1.0 to 1.0)
    pub fn update_axis(&mut self, axis_index: u32, value: f32) {
        if axis_index < 4 {
            self.axis_state[axis_index as usize] = value.clamp(-1.0, 1.0);
        }
    }

    /// Apply deadzone to analog value
    fn apply_deadzone(&self, value: f32) -> f32 {
        if value.abs() < self.deadzone {
            0.0
        } else {
            // Scale to full range after deadzone
            let sign = value.signum();
            let magnitude = (value.abs() - self.deadzone) / (1.0 - self.deadzone);
            sign * magnitude
        }
    }

    /// Get deadzone value
    pub fn get_deadzone(&self) -> f32 {
        self.deadzone
    }

    /// Set deadzone value (0.0 to 1.0)
    pub fn set_deadzone(&mut self, deadzone: f32) {
        self.deadzone = deadzone.clamp(0.0, 1.0);
    }

    /// Get analog to D-Pad configuration
    pub fn get_analog_to_dpad_config(&self) -> &Option<AnalogToDPad> {
        &self.analog_to_dpad_config
    }

    /// Set analog to D-Pad configuration
    pub fn set_analog_to_dpad_config(&mut self, config: Option<AnalogToDPad>) {
        self.analog_to_dpad_config = config;
    }

    /// Get button mapping
    pub fn get_button_mapping(&self) -> &HashMap<u32, u32> {
        &self.button_mapping
    }

    /// Set button mapping
    pub fn set_button_mapping(&mut self, mapping: HashMap<u32, u32>) {
        self.button_mapping = mapping;
    }

    /// Clear all button and axis state (release everything)
    pub fn clear_state(&mut self) {
        self.button_state = [false; 16];
        self.axis_state = [0.0; 4];
    }
}

impl InputProvider for GamepadInputProvider {
    fn get_state(&self) -> u32 {
        let mut state = 0u32;

        // Physical buttons
        for (gamepad_btn, &console_btn) in &self.button_mapping {
            if *gamepad_btn < 16 && self.button_state[*gamepad_btn as usize] {
                state |= 1 << console_btn;
            }
        }

        // Analog to D-Pad (if configured)
        if let Some(ref config) = self.analog_to_dpad_config {
            let x = self.apply_deadzone(self.axis_state[0]); // LeftX
            let y = self.apply_deadzone(self.axis_state[1]); // LeftY

            if y < -config.threshold {
                state |= 1 << 0; // UP (button 0)
            }
            if y > config.threshold {
                state |= 1 << 1; // DOWN (button 1)
            }
            if x < -config.threshold {
                state |= 1 << 2; // LEFT (button 2)
            }
            if x > config.threshold {
                state |= 1 << 3; // RIGHT (button 3)
            }
        }

        state
    }

    fn is_pressed(&self, button: u32) -> bool {
        (self.get_state() & (1 << button)) != 0
    }

    fn get_analog(&self, axis: AnalogAxis) -> f32 {
        let index = match axis {
            AnalogAxis::LeftX => 0,
            AnalogAxis::LeftY => 1,
            AnalogAxis::RightX => 2,
            AnalogAxis::RightY => 3,
            _ => return 0.0,
        };
        self.apply_deadzone(self.axis_state[index])
    }

    fn provider_info(&self) -> InputProviderInfo {
        InputProviderInfo {
            name: format!("Gamepad {}", self.gamepad_id),
            provider_type: InputProviderType::Gamepad,
            id: self.gamepad_id.clone(),
        }
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
        let provider = GamepadInputProvider::new("gamepad-0".to_string());
        assert_eq!(provider.get_state(), 0);
        assert_eq!(
            provider.provider_info().provider_type,
            InputProviderType::Gamepad
        );
        assert_eq!(provider.get_deadzone(), 0.15);
    }

    #[test]
    fn test_button_mapping() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Button 0 (A/Cross) maps to Genesis A (button 6)
        provider.update_button(0, true);
        assert!(provider.is_pressed(6));
    }

    #[test]
    fn test_multiple_buttons() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Press multiple buttons
        provider.update_button(0, true); // A -> button 6
        provider.update_button(1, true); // B -> button 4

        let state = provider.get_state();
        assert!(state & (1 << 6) != 0);
        assert!(state & (1 << 4) != 0);
    }

    #[test]
    fn test_analog_to_dpad_up() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Move left stick up (negative Y)
        provider.update_axis(1, -0.8);

        // Should trigger UP button (button 0)
        assert!(provider.is_pressed(0));
    }

    #[test]
    fn test_analog_to_dpad_down() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Move left stick down (positive Y)
        provider.update_axis(1, 0.8);

        // Should trigger DOWN button (button 1)
        assert!(provider.is_pressed(1));
    }

    #[test]
    fn test_analog_to_dpad_left() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Move left stick left (negative X)
        provider.update_axis(0, -0.8);

        // Should trigger LEFT button (button 2)
        assert!(provider.is_pressed(2));
    }

    #[test]
    fn test_analog_to_dpad_right() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Move left stick right (positive X)
        provider.update_axis(0, 0.8);

        // Should trigger RIGHT button (button 3)
        assert!(provider.is_pressed(3));
    }

    #[test]
    fn test_deadzone() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Set axis value below deadzone (0.15)
        provider.update_axis(0, 0.1);

        // Should NOT trigger RIGHT button
        assert!(!provider.is_pressed(3));

        // Set axis value above deadzone
        provider.update_axis(0, 0.8);

        // Should trigger RIGHT button
        assert!(provider.is_pressed(3));
    }

    #[test]
    fn test_get_analog() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Set LeftX (deadzone will be applied)
        provider.update_axis(0, 0.5);
        let left_x = provider.get_analog(AnalogAxis::LeftX);
        assert!(left_x > 0.0 && left_x <= 1.0);

        // Set LeftY (deadzone will be applied)
        provider.update_axis(1, -0.7);
        let left_y = provider.get_analog(AnalogAxis::LeftY);
        assert!((-1.0..0.0).contains(&left_y));

        // Set RightX (value above deadzone)
        provider.update_axis(2, 0.9);
        let right_x = provider.get_analog(AnalogAxis::RightX);
        assert!(right_x > 0.0 && right_x <= 1.0);

        // Set RightY (value above deadzone)
        provider.update_axis(3, 0.9);
        let right_y = provider.get_analog(AnalogAxis::RightY);
        assert!(right_y > 0.0 && right_y <= 1.0);
    }

    #[test]
    fn test_deadzone_application() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());
        provider.set_deadzone(0.2);

        // Value below deadzone
        provider.update_axis(0, 0.1);
        assert_eq!(provider.get_analog(AnalogAxis::LeftX), 0.0);

        // Value at deadzone boundary
        provider.update_axis(0, 0.2);
        assert_eq!(provider.get_analog(AnalogAxis::LeftX), 0.0);

        // Value above deadzone
        provider.update_axis(0, 0.6);
        let analog = provider.get_analog(AnalogAxis::LeftX);
        assert!(analog > 0.0);
        assert!(analog <= 1.0);
    }

    #[test]
    fn test_set_deadzone() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        provider.set_deadzone(0.3);
        assert_eq!(provider.get_deadzone(), 0.3);

        // Test clamping
        provider.set_deadzone(1.5);
        assert_eq!(provider.get_deadzone(), 1.0);

        provider.set_deadzone(-0.5);
        assert_eq!(provider.get_deadzone(), 0.0);
    }

    #[test]
    fn test_disable_analog_to_dpad() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Disable analog to D-Pad
        provider.set_analog_to_dpad_config(None);

        // Move stick (should NOT trigger D-Pad)
        provider.update_axis(0, 0.8);
        provider.update_axis(1, -0.8);

        // Analog values should still work
        assert_ne!(provider.get_analog(AnalogAxis::LeftX), 0.0);

        // But D-Pad buttons should NOT be triggered by analog
        // (only by physical D-Pad buttons)
        // Note: Physical D-Pad buttons (12-15) would still work
        assert!(provider.get_analog(AnalogAxis::LeftX) != 0.0);
    }

    #[test]
    fn test_provider_info() {
        let provider = GamepadInputProvider::new("gamepad-0".to_string());
        let info = provider.provider_info();

        assert_eq!(info.name, "Gamepad gamepad-0");
        assert_eq!(info.provider_type, InputProviderType::Gamepad);
        assert_eq!(info.id, "gamepad-0");
    }

    #[test]
    fn test_dpad_buttons() {
        let mut provider = GamepadInputProvider::new("gamepad-0".to_string());

        // Test physical D-Pad buttons (12-15)
        provider.update_button(12, true); // D-Pad Up
        assert!(provider.is_pressed(0));

        provider.update_button(12, false);
        provider.update_button(13, true); // D-Pad Down
        assert!(provider.is_pressed(1));

        provider.update_button(13, false);
        provider.update_button(14, true); // D-Pad Left
        assert!(provider.is_pressed(2));

        provider.update_button(14, false);
        provider.update_button(15, true); // D-Pad Right
        assert!(provider.is_pressed(3));
    }
}
