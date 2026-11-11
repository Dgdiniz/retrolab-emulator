//! J-Cart controller-port expander as a coprocessor.
//!
//! J-Cart adds two extra controller ports (ports 3 and 4) memory-mapped
//! into the ROM window at `0x38FFFE` (port 3) and `0x3FFFFE` (port 4),
//! letting four players play without a multitap (Micro Machines 2, Pete
//! Sampras Tennis, Super Skidmarks). Each port uses the standard Genesis
//! TH-multiplexed 3-button protocol.
//!
//! In the composable model the chip is reached through two MMIO windows
//! the plan carries: port 3 with base `0`, port 4 with base
//! [`JCART_PORT4_BASE`]. The chip reads the port from the base-folded
//! offset. Connected-controller button state is driven live by the input
//! layer each frame (via [`JCartChip::set_button`]) and is *not* part of
//! the savestate; only the latched TH/data registers persist.

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};

use crate::cartridge::composable::savestate::{GenesisCoprocessorState, JCartState};

/// MMIO base the plan assigns to the port-4 window, so the chip can tell it
/// apart from the port-3 window (base 0) by the folded offset.
pub const JCART_PORT4_BASE: u32 = 0x40;

/// A J-Cart two-port controller expander.
pub struct JCartChip {
    port3_data: u8,
    port4_data: u8,
    port3_th: bool,
    port4_th: bool,
    /// Live button state (Up, Down, Left, Right, A, B, C, Start), driven by
    /// the input layer, not persisted.
    port3_buttons: [u8; 8],
    port4_buttons: [u8; 8],
    accesses: u64,
    info: ExpansionChipInfo,
}

impl Default for JCartChip {
    fn default() -> Self {
        Self::new()
    }
}

impl JCartChip {
    /// Build a J-Cart expander with both ports idle (buttons released).
    pub fn new() -> Self {
        Self {
            port3_data: 0xFF,
            port4_data: 0xFF,
            port3_th: true,
            port4_th: true,
            port3_buttons: [0; 8],
            port4_buttons: [0; 8],
            accesses: 0,
            info: ExpansionChipInfo {
                name: "J-Cart".to_string(),
                chip_type: ExpansionChipType::Custom(0x0A17),
                description: "Two extra controller ports".to_string(),
                features: vec!["4 players".to_string()],
            },
        }
    }

    /// Set a button on port 3 or 4 (button index 0-7). No-op for other ports.
    pub fn set_button(&mut self, port: u32, button: u8, pressed: bool) {
        let buttons = match port {
            3 => &mut self.port3_buttons,
            4 => &mut self.port4_buttons,
            _ => return,
        };
        if let Some(slot) = buttons.get_mut(button as usize) {
            *slot = u8::from(pressed);
        }
    }

    /// Genesis TH-multiplexed read of one port's controller.
    fn read_controller(buttons: &[u8; 8], th: bool) -> u8 {
        let mut data = 0xFFu8;
        if th {
            // TH=1: Up, Down, Left, Right, B, C on bits 0-5.
            for (bit, idx) in [(0, 0), (1, 1), (2, 2), (3, 3), (4, 5), (5, 6)] {
                if buttons[idx] != 0 {
                    data &= !(1 << bit);
                }
            }
        } else {
            // TH=0: Up, Down on bits 0-1; A, Start on bits 4-5.
            for (bit, idx) in [(0, 0), (1, 1), (4, 4), (5, 7)] {
                if buttons[idx] != 0 {
                    data &= !(1 << bit);
                }
            }
        }
        data
    }

    fn port_for(offset: u32) -> u32 {
        if offset >= JCART_PORT4_BASE {
            4
        } else {
            3
        }
    }
}

impl Coprocessor for JCartChip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
        self.accesses += 1;
        match Self::port_for(offset) {
            3 => Self::read_controller(&self.port3_buttons, self.port3_th),
            _ => Self::read_controller(&self.port4_buttons, self.port4_th),
        }
    }

    fn write(&mut self, offset: u32, value: u8, _master_cycle: u64) {
        self.accesses += 1;
        // Bit 6 is TH.
        if Self::port_for(offset) == 3 {
            self.port3_th = (value & 0x40) != 0;
            self.port3_data = value;
        } else {
            self.port4_th = (value & 0x40) != 0;
            self.port4_data = value;
        }
    }

    fn reset(&mut self, _master_clock_hz: u64) {
        self.port3_data = 0xFF;
        self.port4_data = 0xFF;
        self.port3_th = true;
        self.port4_th = true;
        self.accesses = 0;
    }

    fn serialize(&self) -> Vec<u8> {
        let state = GenesisCoprocessorState::JCart(JCartState {
            port3_data: self.port3_data,
            port4_data: self.port4_data,
            port3_th: self.port3_th,
            port4_th: self.port4_th,
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        if let Ok(GenesisCoprocessorState::JCart(s)) =
            bincode::deserialize::<GenesisCoprocessorState>(data)
        {
            self.port3_data = s.port3_data;
            self.port4_data = s.port4_data;
            self.port3_th = s.port3_th;
            self.port4_th = s.port4_th;
        }
    }

    fn debug_state(&self) -> ExpansionChipState {
        ExpansionChipState {
            chip_type: self.info.chip_type,
            registers: vec![
                ChipRegister {
                    address: 0x38FFFE,
                    name: "PORT3".to_string(),
                    value: u32::from(self.port3_data),
                    description: "controller port 3".to_string(),
                },
                ChipRegister {
                    address: 0x3FFFFE,
                    name: "PORT4".to_string(),
                    value: u32::from(self.port4_data),
                    description: "controller port 4".to_string(),
                },
            ],
            internal_ram: None,
            program_counter: None,
            cycle_count: self.accesses,
        }
    }

    fn label(&self) -> &str {
        &self.info.name
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_default_released() {
        let mut c = JCartChip::new();
        c.write(0, 0x40, 0); // port 3, TH high
        assert_eq!(c.read(0, 0), 0xFF, "no buttons pressed");
        c.write(JCART_PORT4_BASE, 0x40, 0); // port 4, TH high
        assert_eq!(c.read(JCART_PORT4_BASE, 0), 0xFF);
    }

    #[test]
    fn button_press_pulls_bit_low() {
        let mut c = JCartChip::new();
        c.set_button(3, 0, true); // Up on port 3
        c.write(0, 0x40, 0); // TH high
        assert_eq!(c.read(0, 0) & 0x01, 0);
    }

    #[test]
    fn th_multiplexing_selects_button_bank() {
        let mut c = JCartChip::new();
        c.set_button(3, 4, true); // A button

        c.write(0, 0x40, 0); // TH=1: A not visible
        assert_eq!(c.read(0, 0) & 0x10, 0x10);

        c.write(0, 0x00, 0); // TH=0: A visible on bit 4
        assert_eq!(c.read(0, 0) & 0x10, 0x00);
    }

    #[test]
    fn port4_is_independent() {
        let mut c = JCartChip::new();
        c.set_button(4, 1, true); // Down on port 4
        c.write(JCART_PORT4_BASE, 0x40, 0);
        assert_eq!(c.read(JCART_PORT4_BASE, 0) & 0x02, 0);
        // Port 3 unaffected.
        c.write(0, 0x40, 0);
        assert_eq!(c.read(0, 0), 0xFF);
    }

    #[test]
    fn savestate_round_trips_latched_registers() {
        let mut c = JCartChip::new();
        c.write(0, 0x40, 0);
        c.write(JCART_PORT4_BASE, 0x00, 0);
        let bytes = c.serialize();

        let mut other = JCartChip::new();
        other.restore(&bytes, 0);
        assert!(other.port3_th);
        assert!(!other.port4_th);
        assert_eq!(other.debug_state().registers[0].value, 0x40);
    }

    #[test]
    fn reset_clears_latches_and_counter() {
        let mut c = JCartChip::new();
        c.write(0, 0x00, 0);
        assert!(c.accesses() > 0);
        c.reset(0);
        assert!(c.port3_th);
        assert_eq!(c.accesses(), 0);
        assert_eq!(c.label(), "J-Cart");
    }

    #[test]
    fn no_battery_on_jcart() {
        let c = JCartChip::new();
        assert!(c.battery().is_none());
        assert!(!c.battery_dirty());
    }
}
