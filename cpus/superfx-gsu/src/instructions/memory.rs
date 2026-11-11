//! Loads, stores, the immediates and the ROM buffer's instructions.
//!
//! Loads and stores reach Game Pak RAM through the RAM buffer, in the bank
//! RAMBR chooses; a word is a byte and its pair (the address with bit 0
//! flipped), low byte first. Each load or store remembers its address for
//! SBK. The ROM buffer's instructions read the byte at ROMBR:R14, which a
//! write of R14 started fetching.

use retro_core::cpu::Addressable;

use crate::registers::plot_options;
use crate::Gsu;

impl Gsu {
    /// `$30-$3B` STW: store the source word at the RAM address in the
    /// register the nibble names. Under ALT1, STB: its low byte only.
    pub(crate) fn store(&mut self, bus: &mut dyn Addressable, n: usize) {
        let address = self.registers[n];
        self.last_ram_address = address;
        let value = self.source_value();
        if self.alt1() {
            self.queue_ram_write(bus, address, value as u8);
        } else {
            self.write_ram_word(bus, address, value);
        }
        self.clear_prefixes();
    }

    /// `$40-$4B` LDW: load the destination from the RAM address in the
    /// register the nibble names. Under ALT1, LDB: one byte.
    pub(crate) fn load(&mut self, bus: &mut dyn Addressable, n: usize) {
        let address = self.registers[n];
        self.last_ram_address = address;
        let value = if self.alt1() {
            u16::from(self.read_ram(bus, address))
        } else {
            self.read_ram_word(bus, address)
        };
        self.write_destination(value);
        self.clear_prefixes();
    }

    /// `$90` SBK: store the source word back at the last load's or store's
    /// address.
    pub(crate) fn store_back(&mut self, bus: &mut dyn Addressable) {
        let value = self.source_value();
        self.write_ram_word(bus, self.last_ram_address, value);
        self.clear_prefixes();
    }

    /// `$A0-$AF` IBT: load the register the nibble names with the next
    /// byte, its sign extended. Under ALT1, LMS: load it from the RAM
    /// address twice the next byte; under ALT2, SMS: store it there.
    pub(crate) fn byte_immediate_or_short_address(&mut self, bus: &mut dyn Addressable, n: usize) {
        if self.alt1() || self.alt2() {
            let address = u16::from(self.take_operand(bus)) << 1;
            self.access_word(bus, n, address);
        } else {
            let value = self.take_operand(bus) as i8 as u16;
            self.write_register(n, value);
        }
        self.clear_prefixes();
    }

    /// `$F0-$FF` IWT: load the register the nibble names with the next
    /// word. Under ALT1, LM: load it from the RAM address in the next
    /// word; under ALT2, SM: store it there.
    pub(crate) fn word_immediate_or_address(&mut self, bus: &mut dyn Addressable, n: usize) {
        let low = self.take_operand(bus);
        let high = self.take_operand(bus);
        let word = u16::from_le_bytes([low, high]);
        if self.alt1() || self.alt2() {
            self.access_word(bus, n, word);
        } else {
            self.write_register(n, word);
        }
        self.clear_prefixes();
    }

    /// The load (ALT1, it wins when both are set) or the store (ALT2) of
    /// LMS/SMS and LM/SM: register `n` and the word at `address`.
    fn access_word(&mut self, bus: &mut dyn Addressable, n: usize, address: u16) {
        self.last_ram_address = address;
        if self.alt1() {
            let value = self.read_ram_word(bus, address);
            self.write_register(n, value);
        } else {
            let value = self.registers[n];
            self.write_ram_word(bus, address, value);
        }
    }

    /// `$DF` GETC: COLR takes the ROM buffer's byte, as COLOR would. Under
    /// ALT2, RAMB: the source picks the RAM bank; under ALT3, ROMB: the
    /// source picks the ROM bank. Each waits for its buffer first.
    pub(crate) fn color_or_bank(&mut self, bus: &mut dyn Addressable) {
        if !self.alt2() {
            let byte = self.rom_buffer(bus);
            self.color = self.filter_color(byte);
        } else if !self.alt1() {
            self.finish_ram_write(bus);
            self.ram_bank = self.source_value() & 1 != 0;
        } else {
            self.finish_rom_read(bus);
            self.rom_bank = (self.source_value() & 0x7f) as u8;
        }
        self.clear_prefixes();
    }

    /// `$EF` GETB: the destination takes the ROM buffer's byte. Under ALT1,
    /// GETBH: as its high byte, the source's low byte below it; under ALT2,
    /// GETBL: as its low byte, the source's high byte above it; under ALT3,
    /// GETBS: with its sign extended.
    pub(crate) fn get_byte(&mut self, bus: &mut dyn Addressable) {
        let byte = self.rom_buffer(bus);
        let source = self.source_value();
        let value = match (self.alt1(), self.alt2()) {
            (false, false) => u16::from(byte),
            (true, false) => u16::from_le_bytes([source as u8, byte]),
            (false, true) => (source & 0xff00) | u16::from(byte),
            (true, true) => byte as i8 as u16,
        };
        self.write_destination(value);
        self.clear_prefixes();
    }

    /// The colour COLOR and GETC give COLR from `source`: under HIGH
    /// NIBBLE, the source's high nibble; under FREEZE HIGH, its low one,
    /// both keeping COLR's high nibble; otherwise the whole byte.
    pub(crate) fn filter_color(&self, source: u8) -> u8 {
        if self.plot_options & plot_options::HIGH_NIBBLE != 0 {
            (self.color & 0xf0) | (source >> 4)
        } else if self.plot_options & plot_options::FREEZE_HIGH != 0 {
            (self.color & 0xf0) | (source & 0x0f)
        } else {
            source
        }
    }
}
