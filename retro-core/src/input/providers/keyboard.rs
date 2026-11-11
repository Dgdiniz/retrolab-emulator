//! Keyboard input provider
//!
//! Maps keyboard keys to console buttons using configurable mappings.

use crate::input::{AnalogAxis, InputProvider, InputProviderInfo, InputProviderType};
use std::collections::HashMap;

/// Keyboard input provider
///
/// Tracks key states and maps them to button indices.
/// Default mapping is configured for Genesis, but can be customized.
pub struct KeyboardInputProvider {
    /// Current key states (keycode -> pressed)
    key_state: HashMap<String, bool>,

    /// Button mapping (keycode -> button index)
    button_mapping: HashMap<String, u32>,

    /// Optional custom ID override (for split keyboard: "keyboard-p1", "keyboard-p2")
    custom_id: Option<String>,
}

impl KeyboardInputProvider {
    /// Create a new keyboard provider with default Genesis mapping
    pub fn new() -> Self {
        Self {
            key_state: HashMap::new(),
            button_mapping: Self::default_genesis_mapping(),
            custom_id: None,
        }
    }

    /// Create a new keyboard provider with custom mapping
    pub fn with_mapping(button_mapping: HashMap<String, u32>) -> Self {
        Self {
            key_state: HashMap::new(),
            button_mapping,
            custom_id: None,
        }
    }

    /// Set a custom ID (used for split keyboard providers)
    pub fn with_id(mut self, id: String) -> Self {
        self.custom_id = Some(id);
        self
    }

    /// Default Genesis button mapping
    ///
    /// Maps keyboard keys to Genesis button indices:
    /// - Arrow keys -> D-Pad (0-3: Up, Down, Left, Right)
    /// - A, S, D -> A, B, C buttons (6, 4, 5)
    /// - Q, W, E -> X, Y, Z buttons (10, 9, 8)
    /// - Enter -> Start (7)
    /// - Shift -> Mode (11)
    fn default_genesis_mapping() -> HashMap<String, u32> {
        let mut map = HashMap::new();

        // D-Pad (arrow keys)
        map.insert("ArrowUp".to_string(), 0);
        map.insert("ArrowDown".to_string(), 1);
        map.insert("ArrowLeft".to_string(), 2);
        map.insert("ArrowRight".to_string(), 3);

        // Face buttons (A, S, D)
        map.insert("KeyA".to_string(), 6); // A button
        map.insert("KeyS".to_string(), 4); // B button
        map.insert("KeyD".to_string(), 5); // C button

        // Extra 6-button (Q, W, E)
        map.insert("KeyQ".to_string(), 10); // X button
        map.insert("KeyW".to_string(), 9); // Y button
        map.insert("KeyE".to_string(), 8); // Z button

        // Start/Mode
        map.insert("Enter".to_string(), 7); // Start
        map.insert("ShiftLeft".to_string(), 11); // Mode

        map
    }

    /// Set key state
    ///
    /// # Arguments
    /// * `keycode` - JavaScript KeyboardEvent.code (e.g., "KeyA", "ArrowUp")
    /// * `pressed` - Whether the key is pressed
    pub fn set_key(&mut self, keycode: String, pressed: bool) {
        self.key_state.insert(keycode, pressed);
    }

    /// Clear all key states
    pub fn clear(&mut self) {
        self.key_state.clear();
    }

    /// Get button mapping
    pub fn get_button_mapping(&self) -> &HashMap<String, u32> {
        &self.button_mapping
    }

    /// Set button mapping
    pub fn set_button_mapping(&mut self, mapping: HashMap<String, u32>) {
        self.button_mapping = mapping;
    }
}

impl Default for KeyboardInputProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl InputProvider for KeyboardInputProvider {
    fn get_state(&self) -> u32 {
        let mut state = 0u32;

        for (keycode, &pressed) in &self.key_state {
            if pressed {
                if let Some(&button) = self.button_mapping.get(keycode) {
                    state |= 1 << button;
                }
            }
        }

        state
    }

    fn is_pressed(&self, button: u32) -> bool {
        (self.get_state() & (1 << button)) != 0
    }

    fn get_analog(&self, _axis: AnalogAxis) -> f32 {
        0.0 // Keyboard has no analog input
    }

    fn provider_info(&self) -> InputProviderInfo {
        let (id, name) = match &self.custom_id {
            Some(cid) => (cid.clone(), format!("Keyboard ({})", cid)),
            None => ("keyboard".to_string(), "Keyboard".to_string()),
        };
        InputProviderInfo {
            name,
            provider_type: InputProviderType::Keyboard,
            id,
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
        let provider = KeyboardInputProvider::new();
        assert_eq!(provider.get_state(), 0);
        assert_eq!(
            provider.provider_info().provider_type,
            InputProviderType::Keyboard
        );
    }

    #[test]
    fn test_set_key() {
        let mut provider = KeyboardInputProvider::new();
        provider.set_key("ArrowUp".to_string(), true);

        let state = provider.get_state();
        assert!(state & (1 << 0) != 0); // Button 0 (UP) should be pressed
    }

    #[test]
    fn test_default_mapping_arrow_keys() {
        let mut provider = KeyboardInputProvider::new();

        // Test all D-Pad directions
        provider.set_key("ArrowUp".to_string(), true);
        assert!(provider.is_pressed(0)); // UP

        provider.clear();
        provider.set_key("ArrowDown".to_string(), true);
        assert!(provider.is_pressed(1)); // DOWN

        provider.clear();
        provider.set_key("ArrowLeft".to_string(), true);
        assert!(provider.is_pressed(2)); // LEFT

        provider.clear();
        provider.set_key("ArrowRight".to_string(), true);
        assert!(provider.is_pressed(3)); // RIGHT
    }

    #[test]
    fn test_default_mapping_face_buttons() {
        let mut provider = KeyboardInputProvider::new();

        // A button (KeyA -> button 6)
        provider.set_key("KeyA".to_string(), true);
        assert!(provider.is_pressed(6));

        provider.clear();

        // B button (KeyS -> button 4)
        provider.set_key("KeyS".to_string(), true);
        assert!(provider.is_pressed(4));

        provider.clear();

        // C button (KeyD -> button 5)
        provider.set_key("KeyD".to_string(), true);
        assert!(provider.is_pressed(5));
    }

    #[test]
    fn test_default_mapping_6button_extras() {
        let mut provider = KeyboardInputProvider::new();

        // X button (KeyQ -> button 10)
        provider.set_key("KeyQ".to_string(), true);
        assert!(provider.is_pressed(10));

        provider.clear();

        // Y button (KeyW -> button 9)
        provider.set_key("KeyW".to_string(), true);
        assert!(provider.is_pressed(9));

        provider.clear();

        // Z button (KeyE -> button 8)
        provider.set_key("KeyE".to_string(), true);
        assert!(provider.is_pressed(8));
    }

    #[test]
    fn test_default_mapping_start_mode() {
        let mut provider = KeyboardInputProvider::new();

        // Start (Enter -> button 7)
        provider.set_key("Enter".to_string(), true);
        assert!(provider.is_pressed(7));

        provider.clear();

        // Mode (ShiftLeft -> button 11)
        provider.set_key("ShiftLeft".to_string(), true);
        assert!(provider.is_pressed(11));
    }

    #[test]
    fn test_multiple_keys_pressed() {
        let mut provider = KeyboardInputProvider::new();

        // Press UP and A simultaneously
        provider.set_key("ArrowUp".to_string(), true);
        provider.set_key("KeyA".to_string(), true);

        let state = provider.get_state();
        assert!(state & (1 << 0) != 0); // UP
        assert!(state & (1 << 6) != 0); // A
    }

    #[test]
    fn test_clear() {
        let mut provider = KeyboardInputProvider::new();
        provider.set_key("ArrowUp".to_string(), true);
        provider.set_key("KeyA".to_string(), true);

        assert_ne!(provider.get_state(), 0);

        provider.clear();
        assert_eq!(provider.get_state(), 0);
    }

    #[test]
    fn test_custom_mapping() {
        let mut mapping = HashMap::new();
        mapping.insert("Space".to_string(), 0); // Map Space to button 0

        let mut provider = KeyboardInputProvider::with_mapping(mapping);
        provider.set_key("Space".to_string(), true);

        assert!(provider.is_pressed(0));
    }

    #[test]
    fn test_analog_returns_zero() {
        let provider = KeyboardInputProvider::new();

        // Keyboard has no analog input
        assert_eq!(provider.get_analog(AnalogAxis::LeftX), 0.0);
        assert_eq!(provider.get_analog(AnalogAxis::LeftY), 0.0);
    }

    #[test]
    fn test_provider_info() {
        let provider = KeyboardInputProvider::new();
        let info = provider.provider_info();

        assert_eq!(info.name, "Keyboard");
        assert_eq!(info.provider_type, InputProviderType::Keyboard);
        assert_eq!(info.id, "keyboard");
    }
}
