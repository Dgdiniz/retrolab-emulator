//! Genesis I/O system
//!
//! Implements the Sega Genesis I/O ports for controller input.
//!
//! # Hardware
//! - 3 controller ports (Port 1, Port 2, Expansion)
//! - Data registers: 0xA10003, 0xA10005, 0xA10007
//! - Control registers: 0xA10009, 0xA1000B, 0xA1000D
//! - Version register: 0xA10001
//!
//! # Protocol
//! - 3-button: TH multiplexing (2 reads)
//! - 6-button: TH cycle sequence (4+ reads with timeout)

pub mod controller_port;
pub mod devices;

use crate::io::controller_port::GenesisControllerPort;
use retro_core::cpu::Clockable;

/// Genesis I/O system
///
/// Manages the 3 controller ports and their I/O registers
pub struct GenesisIo {
    /// Controller ports (Port 1, 2, Expansion)
    ports: [GenesisControllerPort; 3],

    /// Data registers (0xA10003, 0xA10005, 0xA10007)
    data_registers: [u8; 3],

    /// Control registers (0xA10009, 0xA1000B, 0xA1000D)
    control_registers: [u8; 3],

    /// Cycle tracking (for 6-button timeout)
    port_cycles: [u32; 3],
}

impl GenesisIo {
    pub fn new() -> Self {
        Self {
            ports: [
                GenesisControllerPort::new(0),
                GenesisControllerPort::new(1),
                GenesisControllerPort::new(2),
            ],
            // Port A/B/C DATA power on as 0 (as in Genesis Plus GX),
            // so the first TH=1 write after CTRL selects TH-as-output is a
            // real 0->1 edge (6-button phase advance). Starting at 0xFF would
            // hide that edge and make Contra's pad snapshot look like a 3-button.
            data_registers: [0x00, 0x00, 0x00],
            control_registers: [0x00, 0x00, 0x00],
            port_cycles: [0, 0, 0],
        }
    }

    /// Read from I/O register (called by AddressBus)
    ///
    /// I/O registers are mapped at odd addresses. Even addresses return 0x00
    /// here; MAME and Genesis Plus GX read the same register there.
    pub fn read(&mut self, addr: u32) -> u8 {
        match addr {
            // Version register (odd address)
            0xA10001 => self.read_version_register(),
            // Data registers (odd addresses)
            0xA10003 => self.read_data_register(0),
            0xA10005 => self.read_data_register(1),
            0xA10007 => self.read_data_register(2),
            // Control registers (odd addresses)
            0xA10009 => self.control_registers[0],
            0xA1000B => self.control_registers[1],
            0xA1000D => self.control_registers[2],
            // Serial control/data registers (odd addresses, directly readable)
            0xA1000F => 0x00, // S-Ctrl 1
            0xA10011 => 0xFF, // TxData 1 (no data)
            0xA10013 => 0x00, // RxData 1
            0xA10015 => 0x00, // S-Ctrl 2
            0xA10017 => 0xFF, // TxData 2
            0xA10019 => 0x00, // RxData 2
            0xA1001B => 0x00, // S-Ctrl 3
            0xA1001D => 0xFF, // TxData 3
            0xA1001F => 0x00, // RxData 3
            // Even addresses and unmapped odd addresses return 0x00
            _ => 0x00,
        }
    }

    /// Write to I/O register
    pub fn write(&mut self, addr: u32, value: u8) {
        match addr {
            0xA10003 => self.write_data_register(0, value),
            0xA10005 => self.write_data_register(1, value),
            0xA10007 => self.write_data_register(2, value),
            0xA10009 => self.write_control_register(0, value),
            0xA1000B => self.write_control_register(1, value),
            0xA1000D => self.write_control_register(2, value),
            _ => {}
        }
    }

    /// Read version register
    ///
    /// Bit 7: MODE - Overseas (0=Domestic/Japan, 1=Overseas/US+EU)
    /// Bit 6: VMOD - Video mode (0=NTSC, 1=PAL)
    /// Bit 5: DISK - Expansion unit (0=connected, 1=none)
    /// Bits 4-0: VER - Hardware version
    fn read_version_register(&self) -> u8 {
        // 0xA0 = 1010 0000
        // - Overseas (bit 7 = 1)
        // - NTSC (bit 6 = 0)
        // - No expansion unit (bit 5 = 1), MAME's no-SCD default
        // - Version 0 (bits 4-0 = 0)
        0xA0
    }

    /// Read data register
    ///
    /// Returns merged value of output pins (from data register)
    /// and input pins (from connected device)
    fn read_data_register(&mut self, port: usize) -> u8 {
        if port >= 3 {
            return 0xFF;
        }

        let control = self.control_registers[port];

        // TH pin: driven from DATA when CTRL marks it an output, else pulled high.
        let th_value = if (control & 0x40) != 0 {
            (self.data_registers[port] & 0x40) != 0
        } else {
            true
        };

        // Read from connected device
        let device_data = self.ports[port].read(th_value);

        // Merge with control register
        // Bits set to output (1 in control) return written value
        // Bits set to input (0 in control) return device value
        // Only bits 0-6 are valid; bit 7 always reads as 0
        let mut result = 0u8;
        for bit in 0..7 {
            let mask = 1 << bit;
            if (control & mask) != 0 {
                // Output: return written value
                result |= self.data_registers[port] & mask;
            } else {
                // Input: return device value
                result |= device_data & mask;
            }
        }

        result
    }

    /// Write control register
    ///
    /// When the control byte changes, re-apply the current DATA value through
    /// the pad write path. Selecting TH as an
    /// output while DATA.TH is still 0 forces the pad's latched TH low so the
    /// next TH=1 write is a rising edge.
    fn write_control_register(&mut self, port: usize, value: u8) {
        if port >= 3 {
            return;
        }
        if self.control_registers[port] == value {
            return;
        }
        self.control_registers[port] = value;
        // Re-drive DATA into the pad with the new direction mask.
        let data = self.data_registers[port];
        self.write_data_register(port, data);
    }

    /// Write data register
    ///
    /// Advances the 6-button phase on TH **0->1** only.
    fn write_data_register(&mut self, port: usize, value: u8) {
        if port >= 3 {
            return;
        }

        let old_th = (self.data_registers[port] & 0x40) != 0;
        // Only the TH output bit participates in the pad handshake when the
        // corresponding CTRL bit marks TH as an output. When TH
        // is an input, the pin is pulled high and no phase advance occurs.
        let th_is_output = (self.control_registers[port] & 0x40) != 0;
        let new_th = if th_is_output {
            (value & 0x40) != 0
        } else {
            true // pulled high
        };

        if th_is_output && new_th && !old_th {
            self.ports[port].on_th_rising();
            self.port_cycles[port] = 0;
        } else if th_is_output && old_th != new_th {
            // Falling edge: keep the 1.5 ms timeout window alive.
            self.port_cycles[port] = 0;
        }

        self.data_registers[port] = value;
    }

    /// Sync button states from InputManager into connected devices.
    /// Called once per frame before CPU execution.
    pub fn sync_input(&mut self, port0_state: u32, port1_state: u32) {
        self.ports[0].set_device_button_state(port0_state);
        self.ports[1].set_device_button_state(port1_state);
    }

    /// Get mutable reference to a port (for connecting devices)
    pub fn port_mut(&mut self, port: usize) -> Option<&mut GenesisControllerPort> {
        if port < 3 {
            Some(&mut self.ports[port])
        } else {
            None
        }
    }

    /// Get reference to a port
    pub fn port(&self, port: usize) -> Option<&GenesisControllerPort> {
        if port < 3 {
            Some(&self.ports[port])
        } else {
            None
        }
    }
}

impl Clockable for GenesisIo {
    fn clock(&mut self, cycles: u32) -> u32 {
        // Clock each port for 6-button timeout
        // Timeout: 1.5ms ~= 11,505 cycles @ 7.67 MHz
        // Using 12000 as a safe threshold
        for i in 0..3 {
            self.port_cycles[i] += cycles;
            if self.port_cycles[i] > 12000 {
                self.ports[i].reset_timeout();
                self.port_cycles[i] = 0;
            }
        }
        cycles
    }
}

impl Default for GenesisIo {
    fn default() -> Self {
        Self::new()
    }
}

// Savestate support
use crate::system::savestate::IoState;

impl GenesisIo {
    pub fn to_savestate(&self) -> IoState {
        IoState {
            data_registers: self.data_registers,
            control_registers: self.control_registers,
            port_cycles: self.port_cycles,
            th_toggle_counts: [
                self.ports[0].th_toggle_count(),
                self.ports[1].th_toggle_count(),
                self.ports[2].th_toggle_count(),
            ],
        }
    }

    pub fn from_savestate(&mut self, s: &IoState) {
        self.data_registers = s.data_registers;
        self.control_registers = s.control_registers;
        self.port_cycles = s.port_cycles;
        // `to_savestate` captures the per-port TH toggle counts (6-button
        // polling phase) and they are part of the hashed state, so they MUST
        // be restored here: a port left at 0 desyncs any game that has polled
        // a pad since the last timeout.
        for (port, &count) in self.ports.iter_mut().zip(s.th_toggle_counts.iter()) {
            port.set_th_toggle_count(count);
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
    fn test_version_register() {
        let mut io = GenesisIo::new();
        assert_eq!(io.read(0xA10001), 0xA0); // Overseas NTSC, no expansion unit
    }

    #[test]
    fn test_data_register_no_device() {
        let mut io = GenesisIo::new();
        // No device connected, control = 0 (all inputs)
        // Should return 0x7F (bits 0-6, TH included, pulled high; bit 7 reads 0)
        // Note: With no device, the port returns pulled-high values
        let result = io.read(0xA10003);
        // The exact value depends on device implementation
        assert!(result == 0xFF || result == 0x7F);
    }

    #[test]
    fn test_control_register_write_read() {
        let mut io = GenesisIo::new();
        io.write(0xA10009, 0x40); // Set bit 6 (TH output)
        assert_eq!(io.read(0xA10009), 0x40);
    }

    #[test]
    fn test_data_register_loopback() {
        let mut io = GenesisIo::new();
        // Set control to all outputs
        io.write(0xA10009, 0xFF);
        // Write 0x55
        io.write(0xA10003, 0x55);
        // Read back
        assert_eq!(io.read(0xA10003), 0x55);
    }

    #[test]
    fn test_port_mut_access() {
        let mut io = GenesisIo::new();
        assert!(io.port_mut(0).is_some());
        assert!(io.port_mut(1).is_some());
        assert!(io.port_mut(2).is_some());
        assert!(io.port_mut(3).is_none());
    }

    #[test]
    fn test_clockable_timeout() {
        let mut io = GenesisIo::new();
        // Simulate timeout
        io.clock(15000); // > 12000 cycles
                         // Port cycles should reset to 0 after timeout
        assert_eq!(io.port_cycles[0], 0);
    }

    #[test]
    fn contra_style_six_button_poll_gives_the_recorded_sequence() {
        use crate::io::devices::Genesis6ButtonDevice;

        let mut io = GenesisIo::new();
        io.port_mut(0)
            .unwrap()
            .connect_device(Box::new(Genesis6ButtonDevice::new()));

        // CTRL TH-as-output while DATA.TH is still 0 (MD power-on DATA=0).
        io.write(0xA10009, 0x40);

        let mut seq = Vec::new();
        for &th in &[0x40u8, 0x00, 0x40, 0x00, 0x40, 0x00, 0x40, 0x00] {
            io.write(0xA10003, th);
            seq.push(io.read(0xA10003));
        }
        assert_eq!(seq, vec![0x7f, 0x33, 0x7f, 0x30, 0x7f, 0x3f, 0x7f, 0x33]);
    }
}
