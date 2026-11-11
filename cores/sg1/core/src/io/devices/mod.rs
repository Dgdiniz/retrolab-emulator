//! Genesis input devices
//!
//! Implements various Genesis-compatible input devices

pub mod genesis_3button;
pub mod genesis_6button;

pub use genesis_3button::Genesis3ButtonDevice;
pub use genesis_6button::Genesis6ButtonDevice;
