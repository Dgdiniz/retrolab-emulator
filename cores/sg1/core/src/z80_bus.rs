//! Z80 Address Bus
//!
//! The Z80 in the Genesis has access to:
//! - 8KB of dedicated RAM (0x0000-0x1FFF)
//! - PSG via port 0x7F11 (and mirrors)
//! - YM2612 via memory-mapped addresses 0x4000-0x5FFF
//! - Bank register for the 68K window (0x6000-0x60FF)
//! - 68K address space via bank window (0x8000-0xFFFF)

use crate::cartridge::composable::GenesisCartridge;
use crate::memory::GenesisMemory;
use crate::psg::Psg;
use crate::ym2612::Ym2612;
use retro_core::cpu::Addressable;

/// Z80 Address Bus
///
/// Maps Z80 address space to appropriate components.
/// Bank switching allows the Z80 to access 68K address space (ROM, RAM)
/// through a 32KB window at 0x8000-0xFFFF.
pub struct Z80Bus<'a> {
    psg: &'a mut Psg,
    ym2612: &'a mut Ym2612,
    /// 9-bit bank register, shifted left by 15 to form the 68K base address
    bank_register: &'a mut u32,
    /// Bit counter for serial bank register writes (0-8)
    bank_bit_count: &'a mut u8,
    /// Cartridge ROM for bank reads
    cartridge: Option<&'a mut (dyn GenesisCartridge + 'static)>,
    /// Genesis memory (includes Z80 RAM and 68K work RAM for bank reads)
    memory: &'a mut GenesisMemory,
}

impl<'a> Z80Bus<'a> {
    pub fn new(
        psg: &'a mut Psg,
        ym2612: &'a mut Ym2612,
        bank_register: &'a mut u32,
        bank_bit_count: &'a mut u8,
        cartridge: Option<&'a mut (dyn GenesisCartridge + 'static)>,
        memory: &'a mut GenesisMemory,
    ) -> Self {
        Self {
            psg,
            ym2612,
            bank_register,
            bank_bit_count,
            cartridge,
            memory,
        }
    }

    /// Clock the YM2612 for the given number of 68K cycles.
    /// Used for interleaving Z80 and YM2612 execution to ensure
    /// DAC writes are reflected in sample generation promptly.
    pub fn clock_ym2612(&mut self, cycles: u32) {
        use retro_core::cpu::Clockable;
        self.ym2612.clock(cycles);
    }

    /// Clock the PSG for the given number of Z80 cycles.
    pub fn clock_psg(&mut self, cycles: u32) {
        use retro_core::cpu::Clockable;
        self.psg.clock(cycles);
    }

    /// Read from 68K address space via bank window
    /// Z80 address 0x8000-0xFFFF maps to 68K address: bank_register | (z80_addr & 0x7FFF)
    fn read_banked(&mut self, addr: u32) -> u8 {
        let addr_68k = *self.bank_register | (addr & 0x7FFF);

        match addr_68k {
            // Cartridge ROM (0x000000-0x3FFFFF)
            0x000000..=0x3FFFFF => {
                if let Some(cart) = &mut self.cartridge {
                    // The Z80 bank window reads ROM/mapped-ROM only; chips do
                    // not use the master-cycle here, so pass 0.
                    cart.read_bus(addr_68k, 0)
                } else {
                    0xFF
                }
            }
            // Work RAM (0xE00000-0xFFFFFF, mirrored)
            0xE00000..=0xFFFFFF => self.memory.read_work_ram((addr_68k & 0xFFFF) as usize),
            _ => 0xFF,
        }
    }

    /// Write to 68K address space via bank window
    fn write_banked(&mut self, addr: u32, value: u8) {
        let addr_68k = *self.bank_register | (addr & 0x7FFF);

        match addr_68k {
            // Cartridge ROM is read-only, ignore writes
            0x000000..=0x3FFFFF => {}
            // Work RAM (0xE00000-0xFFFFFF, mirrored)
            0xE00000..=0xFFFFFF => {
                self.memory
                    .write_work_ram((addr_68k & 0xFFFF) as usize, value);
            }
            _ => {}
        }
    }
}

impl<'a> Addressable for Z80Bus<'a> {
    fn read(&mut self, addr: u32) -> u8 {
        match addr & 0xFFFF {
            // Z80 RAM (8KB, mirrored)
            0x0000..=0x1FFF => self.memory.z80_ram.read(addr & 0x1FFF),
            0x2000..=0x3FFF => self.memory.z80_ram.read(addr & 0x1FFF), // Mirror

            // YM2612 (memory-mapped at 0x4000-0x5FFF)
            0x4000..=0x5FFF => {
                let port = (addr & 0x03) as u8;
                self.ym2612.read_port(port)
            }

            // Bank register (write-only, reads return 0xFF)
            0x6000..=0x60FF => 0xFF,

            // PSG (write-only, reads return 0xFF)
            0x7F11 => 0xFF,

            // VDP: the Z80's access to the VDP ports is not emulated
            0x7F00..=0x7F1F => 0xFF, // Stub

            // 68K address space via bank window
            0x8000..=0xFFFF => self.read_banked(addr),

            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        match addr & 0xFFFF {
            // Z80 RAM (8KB, mirrored)
            0x0000..=0x1FFF => self.memory.z80_ram.write(addr & 0x1FFF, value),
            0x2000..=0x3FFF => self.memory.z80_ram.write(addr & 0x1FFF, value), // Mirror

            // YM2612 (memory-mapped at 0x4000-0x5FFF)
            0x4000..=0x5FFF => {
                let port = (addr & 0x03) as u8;
                self.ym2612.write_port(port, value);
            }

            // Bank register - serial write, 1 bit at a time
            // Each write shifts in bit 0 of the value
            // After 9 writes, the 9-bit register is complete
            0x6000..=0x60FF => {
                // Shift in bit 0 of written value
                let bit = (value & 0x01) as u32;
                *self.bank_register = (*self.bank_register >> 1) | (bit << 23);
                *self.bank_bit_count += 1;

                if *self.bank_bit_count >= 9 {
                    // After 9 bits: bank_register has the 9-bit value in bits 23-15
                    // Mask to keep only bits 23-15 (the bank address bits)
                    *self.bank_register &= 0xFF8000;
                    *self.bank_bit_count = 0;
                }
            }

            // PSG
            // Genesis Z80 accesses PSG at 0x7F11 (and mirrors with bit 0 = 1)
            0x7F00..=0x7FFF if (addr & 0x01) == 1 => {
                self.psg.write(value);
            }

            // VDP: the Z80's access to the VDP ports is not emulated
            0x7F00..=0x7F1F => {} // Stub

            // 68K address space via bank window
            0x8000..=0xFFFF => self.write_banked(addr, value),

            _ => {}
        }
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        let low = self.read(addr) as u16;
        let high = self.read(addr + 1) as u16;
        (high << 8) | low // Little-endian: the byte at `addr` is the low half
    }

    fn write_word(&mut self, addr: u32, value: u16) {
        // Little-endian, as `read_word`: the low half goes to `addr`
        self.write(addr, value as u8);
        self.write(addr + 1, (value >> 8) as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::audio::AudioDebugger;

    /// Helper to create a Z80Bus for tests without cartridge
    fn create_test_bus<'a>(
        psg: &'a mut Psg,
        ym2612: &'a mut Ym2612,
        bank_reg: &'a mut u32,
        bank_cnt: &'a mut u8,
        memory: &'a mut GenesisMemory,
    ) -> Z80Bus<'a> {
        Z80Bus::new(psg, ym2612, bank_reg, bank_cnt, None, memory)
    }

    #[test]
    fn test_z80_ram_access() {
        let psg_clock = 3_579_545;
        let ym_clock = 7_670_454;
        let mut psg = Psg::new(psg_clock, 44100);
        let mut ym2612 = Ym2612::new(ym_clock, 44100);
        let mut bank_reg = 0u32;
        let mut bank_cnt = 0u8;
        let mut memory = GenesisMemory::new();
        let mut bus = create_test_bus(
            &mut psg,
            &mut ym2612,
            &mut bank_reg,
            &mut bank_cnt,
            &mut memory,
        );

        // Write to RAM (z80_ram is inside memory)
        bus.write(0x0000, 0x42);
        assert_eq!(bus.read(0x0000), 0x42);

        // Test mirroring
        bus.write(0x2000, 0x55);
        assert_eq!(bus.read(0x0000), 0x55);
    }

    #[test]
    fn test_z80_psg_write() {
        let psg_clock = 3_579_545;
        let ym_clock = 7_670_454;
        let mut psg = Psg::new(psg_clock, 44100);
        let mut ym2612 = Ym2612::new(ym_clock, 44100);
        let mut bank_reg = 0u32;
        let mut bank_cnt = 0u8;
        let mut memory = GenesisMemory::new();

        {
            let mut bus = create_test_bus(
                &mut psg,
                &mut ym2612,
                &mut bank_reg,
                &mut bank_cnt,
                &mut memory,
            );

            bus.write(0x7F11, 0x8E); // Tone 0 frequency low
            bus.write(0x7F11, 0x0F); // Tone 0 frequency high
            bus.write(0x7F11, 0x90); // Tone 0 volume max
        }

        let state = psg.get_channel_state(0).unwrap();
        assert_eq!(state.frequency_raw, 0x0FE);
        assert_eq!(state.attenuation, 0);
    }

    #[test]
    fn test_z80_psg_port_mirrors() {
        let psg_clock = 3_579_545;
        let ym_clock = 7_670_454;
        let mut psg = Psg::new(psg_clock, 44100);
        let mut ym2612 = Ym2612::new(ym_clock, 44100);
        let mut bank_reg = 0u32;
        let mut bank_cnt = 0u8;
        let mut memory = GenesisMemory::new();

        {
            let mut bus = create_test_bus(
                &mut psg,
                &mut ym2612,
                &mut bank_reg,
                &mut bank_cnt,
                &mut memory,
            );

            bus.write(0x7F01, 0x8D);
            bus.write(0x7F11, 0x8D);
            bus.write(0x7F13, 0x8D);
            bus.write(0x7FFF, 0x90); // Volume
        }

        let state = psg.get_channel_state(0).unwrap();
        assert_eq!(state.attenuation, 0);
    }

    #[test]
    fn test_z80_ym2612_write() {
        let psg_clock = 3_579_545;
        let ym_clock = 7_670_454;
        let mut psg = Psg::new(psg_clock, 44100);
        let mut ym2612 = Ym2612::new(ym_clock, 44100);
        let mut bank_reg = 0u32;
        let mut bank_cnt = 0u8;
        let mut memory = GenesisMemory::new();

        {
            let mut bus = create_test_bus(
                &mut psg,
                &mut ym2612,
                &mut bank_reg,
                &mut bank_cnt,
                &mut memory,
            );

            bus.write(0x4000, 0x22); // Address: LFO control
            bus.write(0x4001, 0x08); // Data: Enable LFO
        }

        assert!(ym2612.lfo().is_enabled());
    }

    #[test]
    fn test_z80_ym2612_port_mapping() {
        let psg_clock = 3_579_545;
        let ym_clock = 7_670_454;
        let mut psg = Psg::new(psg_clock, 44100);
        let mut ym2612 = Ym2612::new(ym_clock, 44100);
        let mut bank_reg = 0u32;
        let mut bank_cnt = 0u8;
        let mut memory = GenesisMemory::new();

        {
            let mut bus = create_test_bus(
                &mut psg,
                &mut ym2612,
                &mut bank_reg,
                &mut bank_cnt,
                &mut memory,
            );

            bus.write(0x5000, 0x22); // Should map to port 0
            bus.write(0x5001, 0x00); // Should map to port 1 (disable LFO)
        }

        assert!(!ym2612.lfo().is_enabled());
    }

    #[test]
    fn test_z80_bank_register() {
        let psg_clock = 3_579_545;
        let ym_clock = 7_670_454;
        let mut psg = Psg::new(psg_clock, 44100);
        let mut ym2612 = Ym2612::new(ym_clock, 44100);
        let mut bank_reg = 0u32;
        let mut bank_cnt = 0u8;
        let mut memory = GenesisMemory::new();

        {
            let mut bus = create_test_bus(
                &mut psg,
                &mut ym2612,
                &mut bank_reg,
                &mut bank_cnt,
                &mut memory,
            );

            // Write 9 bits to bank register to set bank to 0x010000 (bit 1 = 1)
            // Bits are shifted in LSB first
            // To get address 0x010000, we need bit 1 set (bank bit 1 = address bit 16)
            // Write: 0,1,0,0,0,0,0,0,0 (9 bits, LSB first)
            bus.write(0x6000, 0x00); // bit 0 = 0
            bus.write(0x6000, 0x01); // bit 1 = 1
            bus.write(0x6000, 0x00); // bit 2 = 0
            bus.write(0x6000, 0x00); // bit 3 = 0
            bus.write(0x6000, 0x00); // bit 4 = 0
            bus.write(0x6000, 0x00); // bit 5 = 0
            bus.write(0x6000, 0x00); // bit 6 = 0
            bus.write(0x6000, 0x00); // bit 7 = 0
            bus.write(0x6000, 0x00); // bit 8 = 0
        }

        // Bank register should now point to 68K address space
        // The 9-bit value 0b000000010 shifted left by 15 = 0x010000
        assert_eq!(bank_reg & 0xFF8000, 0x010000);
    }
}
