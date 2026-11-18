//! Input providers for different input sources
//!
//! This module contains implementations of `InputProvider` for various
//! input sources: keyboard, gamepad, touch, etc.

pub mod gamepad;
pub mod keyboard;
#[cfg(test)]
pub(crate) mod test_maps;

pub use gamepad::GamepadInputProvider;
pub use keyboard::KeyboardInputProvider;
