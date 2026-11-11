//! Input providers for different input sources
//!
//! This module contains implementations of `InputProvider` for various
//! input sources: keyboard, gamepad, touch, etc.

pub mod gamepad;
pub mod keyboard;

pub use gamepad::GamepadInputProvider;
pub use keyboard::KeyboardInputProvider;
