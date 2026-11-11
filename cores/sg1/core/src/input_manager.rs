//! SG1 input manager: thin wrapper around [`retro_core::input::GenericInputManager`].
//!
//! The generic manager (in `retro-core`) does the heavy lifting:
//! provider registry, port assignments, direct overrides, snapshot
//! history, split keyboard, etc. This file does three things on top:
//!
//! 1. Pins the port count at **2** (Mega Drive has a P1 and a P2 DE-9
//!    socket; multitap is a 3rd-party gadget we don't model here).
//! 2. Initialises a default `KeyboardInputProvider` bound to port 0,
//!    which is the historical SG1 "out of the box" UX. Frontends that
//!    want a different layout can reassign after construction.
//! 3. Re-exports `InputSnapshot`, the element type of `get_history`, so
//!    callers can name it from this crate.
//!
//! The delegation pattern (wrapping the generic manager in a tuple
//! struct and forwarding every method) is intentional: it keeps the
//! SG1 public API explicit and grep-able while the actual logic lives
//! one layer down.

use retro_core::input::manager::GenericInputManager;
use retro_core::input::{GamepadInputProvider, InputProviderInfo, KeyboardInputProvider};
use std::collections::HashMap;

pub use retro_core::input::manager::InputSnapshot;

/// SG1-specific manager: 2 ports, keyboard pre-bound to port 0.
pub struct InputManager(GenericInputManager);

impl InputManager {
    pub fn new() -> Self {
        // SG1 boots with one keyboard provider already registered and
        // attached to port 0, which matches every existing call site and
        // saved configuration.
        let mut inner = GenericInputManager::for_2_ports();
        let keyboard = KeyboardInputProvider::new();
        inner.add_keyboard_provider(keyboard);
        inner.assign_provider_to_port("keyboard".to_string(), 0);
        Self(inner)
    }

    pub fn add_keyboard_provider(&mut self, provider: KeyboardInputProvider) {
        self.0.add_keyboard_provider(provider);
    }

    pub fn add_gamepad_provider(&mut self, provider: GamepadInputProvider) {
        self.0.add_gamepad_provider(provider);
    }

    pub fn add_gamepad(&mut self, gamepad_id: &str) -> bool {
        self.0.add_gamepad(gamepad_id)
    }

    pub fn assign_provider_to_port(&mut self, provider_id: String, port: usize) {
        self.0.assign_provider_to_port(provider_id, port);
    }

    pub fn set_provider_port(&mut self, provider_id: &str, port: Option<usize>) {
        self.0.set_provider_port(provider_id, port);
    }

    pub fn provider_port(&self, provider_id: &str) -> Option<usize> {
        self.0.provider_port(provider_id)
    }

    pub fn port_providers(&self, port: usize) -> &[String] {
        self.0.port_providers(port)
    }

    pub fn swap_ports(&mut self, a: usize, b: usize) {
        self.0.swap_ports(a, b);
    }

    pub fn unassign_port(&mut self, port: usize) {
        self.0.unassign_port(port);
    }

    pub fn get_port_state(&self, port: usize) -> u32 {
        self.0.get_port_state(port)
    }

    pub fn set_direct_override(&mut self, port: usize, mask: u32) {
        self.0.set_direct_override(port, mask);
    }

    pub fn get_direct_override(&self, port: usize) -> u32 {
        self.0.get_direct_override(port)
    }

    pub fn clear_direct_override(&mut self, port: usize) {
        self.0.clear_direct_override(port);
    }

    pub fn clear_all_direct_overrides(&mut self) {
        self.0.clear_all_direct_overrides();
    }

    pub fn update_keyboard(&mut self, keycode: String, pressed: bool) {
        self.0.update_keyboard(keycode, pressed);
    }

    pub fn update_gamepad_button(&mut self, gamepad_id: String, button: u32, pressed: bool) {
        self.0.update_gamepad_button(gamepad_id, button, pressed);
    }

    pub fn update_gamepad_axis(&mut self, gamepad_id: String, axis: u32, value: f32) {
        self.0.update_gamepad_axis(gamepad_id, axis, value);
    }

    pub fn snapshot(&mut self, frame: u64) {
        self.0.snapshot(frame);
    }

    pub fn get_history(&self, count: usize) -> &[InputSnapshot] {
        self.0.get_history(count)
    }

    pub fn get_providers(&self) -> Vec<InputProviderInfo> {
        self.0.get_providers()
    }

    pub fn get_port_assignment(&self, port: usize) -> Option<String> {
        self.0.get_port_assignment(port)
    }

    pub fn set_keyboard_mapping(&mut self, mapping: HashMap<String, u32>) {
        self.0.set_keyboard_mapping(mapping);
    }

    pub fn get_keyboard_mapping(&self) -> HashMap<String, u32> {
        self.0.get_keyboard_mapping()
    }

    pub fn clear_keyboard_state(&mut self) {
        self.0.clear_keyboard_state();
    }

    pub fn set_gamepad_mapping(&mut self, gamepad_id: &str, mapping: HashMap<u32, u32>) {
        self.0.set_gamepad_mapping(gamepad_id, mapping);
    }

    pub fn get_gamepad_mapping(&self, gamepad_id: &str) -> HashMap<u32, u32> {
        self.0.get_gamepad_mapping(gamepad_id)
    }

    pub fn set_gamepad_deadzone(&mut self, gamepad_id: &str, deadzone: f32) {
        self.0.set_gamepad_deadzone(gamepad_id, deadzone);
    }

    pub fn get_gamepad_deadzone(&self, gamepad_id: &str) -> f32 {
        self.0.get_gamepad_deadzone(gamepad_id)
    }

    pub fn clear_gamepad_state(&mut self, gamepad_id: &str) {
        self.0.clear_gamepad_state(gamepad_id);
    }

    pub fn enable_split_keyboard(
        &mut self,
        mapping_p1: HashMap<String, u32>,
        mapping_p2: HashMap<String, u32>,
    ) {
        self.0.enable_split_keyboard(mapping_p1, mapping_p2);
    }

    pub fn disable_split_keyboard(&mut self) {
        self.0.disable_split_keyboard();
    }

    pub fn is_split_keyboard_enabled(&self) -> bool {
        self.0.is_split_keyboard_enabled()
    }
}

impl Default for InputManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    //! Minimal SG1-specific sanity tests. The exhaustive coverage
    //! lives in [`retro_core::input::manager::tests`]; these check the
    //! SG1 shim's own choices (2 ports, keyboard pre-registered and pinned
    //! to port 0) and drive a few forwarding methods (a first gamepad
    //! joining the keyboard on port 0, a port move, a swap, an added pad).

    use super::*;

    #[test]
    fn sg1_init_registers_keyboard_on_port_zero() {
        let manager = InputManager::new();
        assert_eq!(
            manager.get_port_assignment(0),
            Some("keyboard".to_string()),
            "SG1 must boot with the default keyboard bound to port 0"
        );
        assert_eq!(
            manager.get_port_assignment(1),
            None,
            "port 1 must start empty (no controller plugged in)"
        );
    }

    #[test]
    fn sg1_keyboard_drives_port_zero_state() {
        let mut manager = InputManager::new();
        manager.update_keyboard("ArrowUp".to_string(), true);
        assert_ne!(
            manager.get_port_state(0),
            0,
            "pressing a default-mapped key must change port 0 state"
        );
    }

    #[test]
    fn sg1_first_gamepad_plays_player_one_beside_the_keyboard() {
        let mut manager = InputManager::new();
        manager.update_gamepad_button("gamepad-0".to_string(), 0, true);
        assert_eq!(manager.provider_port("gamepad-0"), Some(0));
        assert_eq!(
            manager.port_providers(0),
            ["keyboard".to_string(), "gamepad-0".to_string()]
        );
        assert_ne!(
            manager.get_port_state(0),
            0,
            "the pad's button must reach port 0"
        );
        manager.set_provider_port("gamepad-0", Some(1));
        manager.swap_ports(0, 1);
        assert_eq!(manager.provider_port("gamepad-0"), Some(0));
        assert!(manager.add_gamepad("gamepad-1"));
    }

    #[test]
    fn sg1_has_exactly_two_ports_addressable() {
        // Out-of-range ports must be silently inert; this is the
        // contract the SG1 wasm bridge / debugger UI rely on.
        let manager = InputManager::new();
        assert_eq!(manager.get_port_state(2), 0);
        assert_eq!(manager.get_port_assignment(2), None);
    }
}
