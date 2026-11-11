//! # SG1 Core
//!
//! Sega Genesis (Mega Drive) emulator core implementation.
//!
//! This crate implements the `retro-core::EmulatorCore` trait for the
//! Sega Genesis, providing full emulation of the console's hardware.

// Module structure matching hardware components
pub mod bus;
pub mod cartridge;
pub mod cpu68k;
pub mod debugger;
pub mod expr_ctx;
pub mod golden;
pub mod input_manager;
pub mod io;
pub mod memory;
pub mod psg; // SN76489 Programmable Sound Generator
pub mod sound;
pub mod system;
pub mod timing;
pub mod vdp;
pub mod ym2612;
pub mod z80;
pub mod z80_bus;

// Re-export main types
pub use system::GenesisEmulator;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
