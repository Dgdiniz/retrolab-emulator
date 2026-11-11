//! Genesis 6-button controller
//!
//! The Mega Drive 6-button pad's TH protocol, the official MK-1653-50
//! sequence. The pad counts the times TH rises (not every edge) and answers
//! by that count and by TH's level:
//!
//! - after the second rise, TH low -> `SA0000` (D3-D0 forced 0: the pad
//!   announces itself);
//! - after the third rise, TH high -> `CBMXYZ` (the extra buttons), and TH
//!   low -> `SA1111` (D3-D0 forced 1);
//! - any other time, a 3-button pad's answer: `CBRLDU` with TH high,
//!   `SA00DU` with TH low.
//!
//! The answers were checked against Genesis Plus GX.
//!
//! Idle (no buttons) therefore yields the familiar
//! `7F 33 7F 33 7F 30 7F 3F` (or the one-rising-edge-shifted
//! `7F 33 7F 30 7F 3F 7F 33` when TH starts low).

use retro_core::input::{DeviceInfo, DeviceType, InputDevice, InputProvider};

/// Genesis 6-button controller
pub struct Genesis6ButtonDevice {
    /// Button state bitmask
    state: u32,
    /// TH pulse counter: two per rising edge of TH (0, 2, 4, 6, 8), the form
    /// savestates hold.
    th_counter: u32,
}

impl Genesis6ButtonDevice {
    pub fn new() -> Self {
        Self {
            state: 0,
            th_counter: 0,
        }
    }

    // Button indices (bit positions)
    // Compatible with 3-button layout
    pub const BTN_UP: u32 = 0;
    pub const BTN_DOWN: u32 = 1;
    pub const BTN_LEFT: u32 = 2;
    pub const BTN_RIGHT: u32 = 3;
    pub const BTN_B: u32 = 4;
    pub const BTN_C: u32 = 5;
    pub const BTN_A: u32 = 6;
    pub const BTN_START: u32 = 7;
    // 6-button extras
    pub const BTN_Z: u32 = 8;
    pub const BTN_Y: u32 = 9;
    pub const BTN_X: u32 = 10;
    pub const BTN_MODE: u32 = 11;

    /// Get button state
    pub fn get_state(&self) -> u32 {
        self.state
    }

    /// Set button state
    pub fn set_state(&mut self, state: u32) {
        self.state = state;
    }

    /// Get the TH pulse counter (two per rising edge).
    pub fn get_th_toggle_count(&self) -> u32 {
        self.th_counter
    }

    /// Restore TH pulse counter from a savestate.
    pub fn set_th_counter(&mut self, count: u32) {
        self.th_counter = count;
    }

    fn read_th_high_3button(&self) -> u8 {
        // TH=1 : ?1CBRLDU
        let mut data = 0x7Fu8;
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
    }

    /// How many times TH has risen since the pad's timeout.
    fn th_rises(&self) -> u32 {
        self.th_counter / 2
    }

    /// TH=0 with Start on D5 and A on D4 (active low), and the given levels
    /// on D3-D0.
    fn read_start_and_a(&self, low_lines: u8) -> u8 {
        let mut data = 0x30 | low_lines;
        if self.state & (1 << Self::BTN_A) != 0 {
            data &= !0x10;
        }
        if self.state & (1 << Self::BTN_START) != 0 {
            data &= !0x20;
        }
        data
    }

    /// TH=1 : ?1CBMXYZ, C and B on D5-D4, Mode, X, Y and Z on D3-D0
    /// (active low).
    fn read_extra_buttons(&self) -> u8 {
        let lines = [
            (Self::BTN_Z, 0x01u8),
            (Self::BTN_Y, 0x02),
            (Self::BTN_X, 0x04),
            (Self::BTN_MODE, 0x08),
            (Self::BTN_B, 0x10),
            (Self::BTN_C, 0x20),
        ];
        let mut data = 0x7Fu8;
        for (button, line) in lines {
            if self.state & (1 << button) != 0 {
                data &= !line;
            }
        }
        data
    }

    fn read_th_low_3button(&self) -> u8 {
        // TH=0 : ?0SA00DU
        let mut data = 0x3Fu8;
        if self.state & (1 << Self::BTN_UP) != 0 {
            data &= !0x01;
        }
        if self.state & (1 << Self::BTN_DOWN) != 0 {
            data &= !0x02;
        }
        // Bits 2-3 forced low
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

impl Default for Genesis6ButtonDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl InputDevice for Genesis6ButtonDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Genesis6Button
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            name: "Genesis 6-Button Controller".to_string(),
            device_type: DeviceType::Genesis6Button,
            button_count: 12,
            has_analog: false,
        }
    }

    fn update(&mut self, provider: &dyn InputProvider) {
        self.state = provider.get_state();
    }

    fn read_state(&self, control_state: u8) -> u8 {
        let th_high = (control_state & 0x01) != 0;
        match (self.th_rises(), th_high) {
            // TH=0 : ?0SA0000, the low four lines all read 0, which no
            // 3-button pad can answer.
            (2, false) => self.read_start_and_a(0x00),
            // TH=1 : ?1CBMXYZ
            (3, true) => self.read_extra_buttons(),
            // TH=0 : ?0SA1111
            (3, false) => self.read_start_and_a(0x0F),
            (_, true) => self.read_th_high_3button(),
            (_, false) => self.read_th_low_3button(),
        }
    }

    fn reset(&mut self) {
        // Timeout: the pad forgets the pulses it counted.
        self.th_counter = 0;
    }

    fn clock(&mut self, _cycles: u32) {
        // Called on TH 0->1 only (see GenesisIo). After the fourth rise the
        // pad answers as a 3-button pad until the timeout, so the count
        // stops there.
        if self.th_counter < 8 {
            self.th_counter += 2;
        }
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
        let device = Genesis6ButtonDevice::new();
        assert_eq!(device.get_state(), 0);
        assert_eq!(device.get_th_toggle_count(), 0);
        assert_eq!(device.device_type(), DeviceType::Genesis6Button);
    }

    #[test]
    fn test_device_info() {
        let device = Genesis6ButtonDevice::new();
        let info = device.device_info();
        assert_eq!(info.device_type, DeviceType::Genesis6Button);
        assert_eq!(info.button_count, 12);
        assert!(!info.has_analog);
    }

    #[test]
    fn rising_edge_advances_counter_by_two() {
        let mut device = Genesis6ButtonDevice::new();
        device.clock(0);
        assert_eq!(device.get_th_toggle_count(), 2);
        device.clock(0);
        assert_eq!(device.get_th_toggle_count(), 4);
        device.clock(0);
        assert_eq!(device.get_th_toggle_count(), 6);
        device.clock(0);
        assert_eq!(device.get_th_toggle_count(), 8);
        // Cap
        device.clock(0);
        assert_eq!(device.get_th_toggle_count(), 8);
    }

    fn poll_sequence(start_th: bool) -> Vec<u8> {
        let mut device = Genesis6ButtonDevice::new();
        let mut th = start_th;
        let mut seq = Vec::new();
        // Contra: write TH=1,0,1,0,1,0,1,0 then read after each write
        for &next_th in &[true, false, true, false, true, false, true, false] {
            if next_th && !th {
                device.clock(0);
            }
            th = next_th;
            seq.push(device.read_state(if th { 1 } else { 0 }));
        }
        seq
    }

    #[test]
    fn idle_sequence_from_th_high() {
        assert_eq!(
            poll_sequence(true),
            vec![0x7f, 0x33, 0x7f, 0x33, 0x7f, 0x30, 0x7f, 0x3f]
        );
    }

    #[test]
    fn idle_sequence_from_th_low_as_contra_polls() {
        assert_eq!(
            poll_sequence(false),
            vec![0x7f, 0x33, 0x7f, 0x30, 0x7f, 0x3f, 0x7f, 0x33]
        );
    }

    #[test]
    fn test_cycle_0_behaves_like_3button() {
        let mut device = Genesis6ButtonDevice::new();
        device.set_state((1 << Genesis6ButtonDevice::BTN_UP) | (1 << Genesis6ButtonDevice::BTN_B));

        let data = device.read_state(1);
        assert_eq!(data & 0x01, 0x00); // UP pressed
        assert_eq!(data & 0x10, 0x00); // B pressed
    }

    #[test]
    fn test_third_rise_exposes_extra_buttons() {
        let mut device = Genesis6ButtonDevice::new();
        device.set_state((1 << Genesis6ButtonDevice::BTN_X) | (1 << Genesis6ButtonDevice::BTN_Y));

        // X/Y not visible in early 3-button phases (those bits are L/R)
        assert_eq!(device.read_state(1) & 0x0C, 0x0C);

        // After three rising edges, TH=1 reads CBMXYZ
        device.th_counter = 6;
        let data = device.read_state(1);
        assert_eq!(data & 0x04, 0x00); // X on D2
        assert_eq!(data & 0x02, 0x00); // Y on D1
    }

    #[test]
    fn test_second_rise_th_low_forces_low_lines_to_zero() {
        let mut device = Genesis6ButtonDevice::new();
        device.th_counter = 4;
        assert_eq!(device.read_state(0) & 0x0F, 0x00);
    }

    #[test]
    fn test_third_rise_th_low_forces_low_lines_to_one() {
        let mut device = Genesis6ButtonDevice::new();
        device.th_counter = 6;
        assert_eq!(device.read_state(0) & 0x0F, 0x0F);
    }

    /// What each of D6..D0 carries: a button's letter (the line reads 0
    /// while it is held), or a level the pad forces.
    fn answer(layout: &str, state: u32) -> u8 {
        layout.bytes().fold(0u8, |data, line| {
            let button = match line {
                b'0' => return data << 1,
                b'1' => return (data << 1) | 1,
                b'U' => Genesis6ButtonDevice::BTN_UP,
                b'D' => Genesis6ButtonDevice::BTN_DOWN,
                b'L' => Genesis6ButtonDevice::BTN_LEFT,
                b'R' => Genesis6ButtonDevice::BTN_RIGHT,
                b'A' => Genesis6ButtonDevice::BTN_A,
                b'B' => Genesis6ButtonDevice::BTN_B,
                b'C' => Genesis6ButtonDevice::BTN_C,
                b'S' => Genesis6ButtonDevice::BTN_START,
                b'X' => Genesis6ButtonDevice::BTN_X,
                b'Y' => Genesis6ButtonDevice::BTN_Y,
                b'Z' => Genesis6ButtonDevice::BTN_Z,
                b'M' => Genesis6ButtonDevice::BTN_MODE,
                other => panic!("unknown line {}", other as char),
            };
            (data << 1) | u8::from(state & (1 << button) == 0)
        })
    }

    /// The whole answer table of the pad: every count of TH rises it can
    /// hold, both TH levels, every combination of the twelve buttons.
    #[test]
    fn every_answer_of_the_pad() {
        for rises in 0..=4u32 {
            for th_high in [true, false] {
                let layout = match (rises, th_high) {
                    (2, false) => "0SA0000",
                    (3, true) => "1CBMXYZ",
                    (3, false) => "0SA1111",
                    (_, true) => "1CBRLDU",
                    (_, false) => "0SA00DU",
                };
                let mut device = Genesis6ButtonDevice::new();
                for _ in 0..rises {
                    device.clock(0);
                }
                for state in 0..(1u32 << 12) {
                    device.set_state(state);
                    assert_eq!(
                        device.read_state(u8::from(th_high)),
                        answer(layout, state),
                        "rises {rises}, TH {th_high}, buttons {state:#05x}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_reset_clears_counter() {
        let mut device = Genesis6ButtonDevice::new();
        device.th_counter = 6;
        device.reset();
        assert_eq!(device.get_th_toggle_count(), 0);
    }
}
