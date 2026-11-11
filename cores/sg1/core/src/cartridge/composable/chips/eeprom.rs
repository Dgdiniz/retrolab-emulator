//! Serial EEPROM (X24C01/X24C02) as a coprocessor.
//!
//! Genesis EEPROM carts (NBA Jam, Wonder Boy in Monster World, ...) expose a
//! two-wire (I2C-like) serial EEPROM at a single control byte (usually
//! `0x200001`): bit 0 is SDA, bit 1 is SCL on write, and a read returns
//! the current SDA level. The cell array is the cartridge's battery save,
//! surfaced through [`Coprocessor::battery`] so the composable cartridge
//! routes `.srm` to it.
//!
//! The protocol is a state machine: START/STOP edge detection plus
//! per-SCL-edge shifting through device-address -> memory-address ->
//! read/write data phases.

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};

use crate::cartridge::composable::savestate::{
    EepromChipState, EepromPhase, GenesisCoprocessorState,
};

/// A serial EEPROM chip driven over the two-wire control byte.
pub struct EepromChip {
    data: Vec<u8>,
    size: usize,
    phase: EepromPhase,
    sda: bool,
    scl: bool,
    sda_prev: bool,
    scl_prev: bool,
    bit_count: u8,
    current_byte: u8,
    device_addr: u8,
    rw_bit: bool,
    memory_addr: u8,
    dirty: bool,
    accesses: u64,
    info: ExpansionChipInfo,
}

impl EepromChip {
    /// Build an EEPROM chip with `size` bytes (128 for X24C01, 256 for
    /// X24C02). The cell array starts erased (`0xFF`).
    pub fn new(size: usize) -> Self {
        debug_assert!(size == 128 || size == 256, "EEPROM size must be 128 or 256");
        Self {
            data: vec![0xFF; size],
            size,
            phase: EepromPhase::Idle,
            sda: true,
            scl: true,
            sda_prev: true,
            scl_prev: true,
            bit_count: 0,
            current_byte: 0,
            device_addr: 0,
            rw_bit: false,
            memory_addr: 0,
            dirty: false,
            accesses: 0,
            info: ExpansionChipInfo {
                name: "Serial EEPROM".to_string(),
                chip_type: ExpansionChipType::Custom(0x0200),
                description: "I2C serial EEPROM (X24C01/X24C02)".to_string(),
                features: Vec::new(),
            },
        }
    }

    fn detect_start(&mut self) -> bool {
        // START: SDA falls while SCL is high.
        if self.scl && self.scl_prev && self.sda_prev && !self.sda {
            self.phase = EepromPhase::DeviceAddress;
            self.bit_count = 0;
            self.current_byte = 0;
            return true;
        }
        false
    }

    fn detect_stop(&mut self) -> bool {
        // STOP: SDA rises while SCL is high.
        if self.scl && self.scl_prev && !self.sda_prev && self.sda {
            self.phase = EepromPhase::Idle;
            return true;
        }
        false
    }

    fn handle_scl_rising_edge(&mut self) {
        match self.phase {
            EepromPhase::DeviceAddress => {
                self.current_byte = (self.current_byte << 1) | (self.sda as u8);
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.device_addr = self.current_byte >> 1;
                    self.rw_bit = (self.current_byte & 0x01) != 0;
                    self.phase = EepromPhase::DeviceAck;
                    self.bit_count = 0;
                    self.current_byte = 0;
                }
            }
            EepromPhase::MemoryAddress => {
                self.current_byte = (self.current_byte << 1) | (self.sda as u8);
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.memory_addr = self.current_byte % (self.size as u8);
                    self.phase = EepromPhase::MemoryAck;
                    self.bit_count = 0;
                    self.current_byte = 0;
                }
            }
            EepromPhase::WriteData => {
                self.current_byte = (self.current_byte << 1) | (self.sda as u8);
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.data[self.memory_addr as usize] = self.current_byte;
                    self.dirty = true;
                    self.memory_addr = (self.memory_addr + 1) % (self.size as u8);
                    self.phase = EepromPhase::WriteAck;
                    self.bit_count = 0;
                    self.current_byte = 0;
                }
            }
            EepromPhase::ReadData => {
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.memory_addr = (self.memory_addr + 1) % (self.size as u8);
                    self.phase = EepromPhase::ReadAck;
                    self.bit_count = 0;
                }
            }
            _ => {}
        }
    }

    fn handle_scl_falling_edge(&mut self) {
        match self.phase {
            EepromPhase::DeviceAck | EepromPhase::MemoryAck => {
                if self.phase == EepromPhase::DeviceAck {
                    self.phase = EepromPhase::MemoryAddress;
                } else if self.rw_bit {
                    self.current_byte = self.data[self.memory_addr as usize];
                    self.phase = EepromPhase::ReadData;
                    self.bit_count = 0;
                } else {
                    self.phase = EepromPhase::WriteData;
                }
            }
            EepromPhase::WriteAck => {
                self.phase = EepromPhase::WriteData;
            }
            EepromPhase::ReadAck => {
                if !self.sda {
                    self.current_byte = self.data[self.memory_addr as usize];
                    self.phase = EepromPhase::ReadData;
                    self.bit_count = 0;
                } else {
                    self.phase = EepromPhase::Idle;
                }
            }
            _ => {}
        }
    }

    fn update_state(&mut self) {
        if self.detect_start() || self.detect_stop() {
            return;
        }
        let scl_rising = self.scl && !self.scl_prev;
        let scl_falling = !self.scl && self.scl_prev;
        if scl_rising {
            self.handle_scl_rising_edge();
        } else if scl_falling {
            self.handle_scl_falling_edge();
        }
    }

    fn get_sda(&self) -> bool {
        match self.phase {
            // Device pulls SDA low to acknowledge.
            EepromPhase::DeviceAck | EepromPhase::MemoryAck | EepromPhase::WriteAck => false,
            // Output the current data bit, MSB first.
            EepromPhase::ReadData => {
                let bit_pos = 7 - self.bit_count;
                (self.current_byte >> bit_pos) & 0x01 != 0
            }
            // Pull-up keeps the line high otherwise.
            _ => true,
        }
    }
}

impl Coprocessor for EepromChip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, _offset: u32, _master_cycle: u64) -> u8 {
        self.accesses += 1;
        if self.get_sda() {
            0x01
        } else {
            0x00
        }
    }

    fn write(&mut self, _offset: u32, value: u8, _master_cycle: u64) {
        self.accesses += 1;
        // Bit 0 = SDA, bit 1 = SCL.
        self.sda_prev = self.sda;
        self.scl_prev = self.scl;
        self.sda = (value & 0x01) != 0;
        self.scl = (value & 0x02) != 0;
        self.update_state();
    }

    fn reset(&mut self, _master_clock_hz: u64) {
        self.phase = EepromPhase::Idle;
        self.sda = true;
        self.scl = true;
        self.sda_prev = true;
        self.scl_prev = true;
        self.bit_count = 0;
        self.current_byte = 0;
        self.memory_addr = 0;
        self.accesses = 0;
        // The cell array (battery) is not cleared by a reset.
    }

    fn serialize(&self) -> Vec<u8> {
        let state = GenesisCoprocessorState::Eeprom(EepromChipState {
            data: self.data.clone(),
            phase: self.phase,
            sda: self.sda,
            scl: self.scl,
            sda_prev: self.sda_prev,
            scl_prev: self.scl_prev,
            bit_count: self.bit_count,
            current_byte: self.current_byte,
            device_addr: self.device_addr,
            rw_bit: self.rw_bit,
            memory_addr: self.memory_addr,
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        if let Ok(GenesisCoprocessorState::Eeprom(s)) =
            bincode::deserialize::<GenesisCoprocessorState>(data)
        {
            if s.data.len() == self.size {
                self.data = s.data;
                self.phase = s.phase;
                self.sda = s.sda;
                self.scl = s.scl;
                self.sda_prev = s.sda_prev;
                self.scl_prev = s.scl_prev;
                self.bit_count = s.bit_count;
                self.current_byte = s.current_byte;
                self.device_addr = s.device_addr;
                self.rw_bit = s.rw_bit;
                self.memory_addr = s.memory_addr;
            }
        }
    }

    fn debug_state(&self) -> ExpansionChipState {
        ExpansionChipState {
            chip_type: self.info.chip_type,
            registers: vec![
                ChipRegister {
                    address: 0,
                    name: "SDA".to_string(),
                    value: u32::from(self.get_sda()),
                    description: "serial data line".to_string(),
                },
                ChipRegister {
                    address: 1,
                    name: "ADDR".to_string(),
                    value: u32::from(self.memory_addr),
                    description: "memory address pointer".to_string(),
                },
            ],
            internal_ram: Some(self.data.clone()),
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

    fn battery(&self) -> Option<&[u8]> {
        Some(&self.data)
    }

    fn restore_battery(&mut self, data: &[u8]) {
        if data.len() == self.size {
            self.data.copy_from_slice(data);
            self.dirty = false;
        }
    }

    fn battery_dirty(&self) -> bool {
        self.dirty
    }

    fn clear_battery_dirty(&mut self) {
        self.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive one bit onto the bus: set SDA, clock SCL high then low.
    fn clock_bit(chip: &mut EepromChip, bit: bool) {
        let sda = if bit { 0x01 } else { 0x00 };
        chip.write(0, sda, 0); // SCL low, SDA set
        chip.write(0, sda | 0x02, 0); // SCL rising edge (sampled here)
        chip.write(0, sda, 0); // SCL falling edge
    }

    fn start(chip: &mut EepromChip) {
        chip.write(0, 0x03, 0); // SDA high, SCL high
        chip.write(0, 0x02, 0); // SDA falls while SCL high -> START
    }

    #[test]
    fn new_erased_and_battery_exposed() {
        let chip = EepromChip::new(128);
        assert_eq!(chip.battery().unwrap().len(), 128);
        assert!(chip.battery().unwrap().iter().all(|&b| b == 0xFF));
        assert!(!chip.battery_dirty());
        assert_eq!(chip.label(), "Serial EEPROM");
    }

    #[test]
    fn start_condition_enters_device_address() {
        let mut chip = EepromChip::new(128);
        start(&mut chip);
        assert_eq!(chip.phase, EepromPhase::DeviceAddress);
    }

    #[test]
    fn full_write_then_read_cycle() {
        let mut chip = EepromChip::new(128);

        // Write 0x5A to address 0x00.
        start(&mut chip);
        for bit in [false, false, false, false, false, false, false, false] {
            clock_bit(&mut chip, bit); // device address + W (all zero)
        }
        assert_eq!(chip.phase, EepromPhase::MemoryAddress);
        for _ in 0..8 {
            clock_bit(&mut chip, false); // memory address 0x00
        }
        assert_eq!(chip.phase, EepromPhase::WriteData);
        for i in 0..8 {
            clock_bit(&mut chip, (0x5A >> (7 - i)) & 1 != 0); // data 0x5A, MSB first
        }
        assert_eq!(chip.data[0], 0x5A);
        assert!(chip.battery_dirty());

        // Read it back.
        start(&mut chip);
        for _ in 0..7 {
            clock_bit(&mut chip, false); // device address bits
        }
        clock_bit(&mut chip, true); // R bit
        for _ in 0..8 {
            clock_bit(&mut chip, false); // memory address 0x00
        }
        // Now in ReadData with the MSB of 0x5A (0) on SDA; only the phase is checked.
        assert_eq!(chip.phase, EepromPhase::ReadData);
    }

    #[test]
    fn savestate_round_trips_mid_protocol() {
        let mut chip = EepromChip::new(256);
        start(&mut chip);
        for _ in 0..3 {
            clock_bit(&mut chip, true);
        }
        chip.data[10] = 0x77;
        chip.dirty = true;

        let bytes = chip.serialize();
        let mut other = EepromChip::new(256);
        other.restore(&bytes, 0);
        assert_eq!(other.data[10], 0x77);
        assert_eq!(other.phase, chip.phase);
        assert_eq!(other.bit_count, chip.bit_count);
    }

    #[test]
    fn restore_battery_from_srm() {
        let mut chip = EepromChip::new(128);
        let srm = vec![0x33u8; 128];
        chip.restore_battery(&srm);
        assert_eq!(chip.battery().unwrap()[0], 0x33);
        assert!(!chip.battery_dirty());
        // Wrong size is ignored.
        chip.restore_battery(&[0x44; 64]);
        assert_eq!(chip.battery().unwrap()[0], 0x33);
    }

    #[test]
    fn reset_clears_protocol_not_battery() {
        let mut chip = EepromChip::new(128);
        chip.data[0] = 0xAB;
        start(&mut chip);
        assert_eq!(chip.phase, EepromPhase::DeviceAddress);
        chip.reset(0);
        assert_eq!(chip.phase, EepromPhase::Idle);
        assert_eq!(chip.data[0], 0xAB, "battery survives reset");
    }
}
