//! How the GSU reaches memory: the pipeline and the instruction cache, the
//! ROM and RAM buffers, and the clocks they cost.
//!
//! Code is fetched one byte ahead (the pipeline) through a 512-byte cache
//! of the code from CBR on: a line of 16 bytes is loaded from the bus the
//! first time one of its bytes is needed, and read from the cache after
//! that; code outside the cache's 512 bytes is read from the bus each time.
//! The ROM buffer reads the byte at ROMBR:R14 a few clocks after R14 is
//! written, and the RAM buffer writes a stored byte a few clocks after the
//! store; using either buffer, or fetching code from its memory, first
//! waits for its transfer to land. A cache read takes the clock divider's
//! clocks (one or two), a bus access four more.
//!
//! This follows ares (`sfc/coprocessor/superfx/memory.cpp`, `timing.cpp`),
//! ISC, the notice is at the end of `LICENSE`.

use retro_core::cpu::Addressable;

use crate::registers::status;
use crate::Gsu;

/// Where Game Pak RAM starts on the GSU's bus.
const RAM_START: u32 = 0x70_0000;

impl Gsu {
    /// The clocks a read from the instruction cache takes.
    #[inline]
    pub(crate) fn cache_clocks(&self) -> u32 {
        u32::from(self.clock_divider)
    }

    /// The clocks an access to Game Pak ROM or RAM takes.
    #[inline]
    pub(crate) fn memory_clocks(&self) -> u32 {
        u32::from(self.clock_divider) + 4
    }

    /// The address of `offset` in the current bank of Game Pak RAM.
    #[inline]
    pub(crate) fn ram_location(&self, offset: u16) -> u32 {
        RAM_START + (u32::from(self.ram_bank) << 16) + u32::from(offset)
    }

    /// Let `clocks` clocks pass: the buffers move on, and a transfer whose
    /// time is up lands, the ROM buffer takes the byte at ROMBR:R14 as
    /// they are now, the RAM buffer writes its byte into the current bank.
    pub(crate) fn spend(&mut self, bus: &mut dyn Addressable, clocks: u32) {
        if self.rom_read_delay != 0 {
            self.rom_read_delay -= clocks.min(u32::from(self.rom_read_delay)) as u8;
            if self.rom_read_delay == 0 {
                self.set_flag(status::ROM_READ, false);
                let address = (u32::from(self.rom_bank) << 16) + u32::from(self.registers[14]);
                self.rom_data = bus.read(address);
            }
        }
        if self.ram_write_delay != 0 {
            self.ram_write_delay -= clocks.min(u32::from(self.ram_write_delay)) as u8;
            if self.ram_write_delay == 0 {
                let address = self.ram_location(self.ram_write_address);
                bus.write(address, self.ram_write_data);
            }
        }
        self.clocks += clocks;
        self.cycles += u64::from(clocks);
    }

    /// Start a read through the ROM buffer, as writing R14 does.
    #[inline]
    pub(crate) fn start_rom_read(&mut self) {
        self.set_flag(status::ROM_READ, true);
        self.rom_read_delay = self.memory_clocks() as u8;
    }

    /// Wait for the ROM buffer's read to land.
    #[inline]
    pub(crate) fn finish_rom_read(&mut self, bus: &mut dyn Addressable) {
        if self.rom_read_delay != 0 {
            let remaining = u32::from(self.rom_read_delay);
            self.spend(bus, remaining);
        }
    }

    /// The ROM buffer's byte, once its read has landed.
    #[inline]
    pub(crate) fn rom_buffer(&mut self, bus: &mut dyn Addressable) -> u8 {
        self.finish_rom_read(bus);
        self.rom_data
    }

    /// Wait for the RAM buffer's write to land.
    #[inline]
    pub(crate) fn finish_ram_write(&mut self, bus: &mut dyn Addressable) {
        if self.ram_write_delay != 0 {
            let remaining = u32::from(self.ram_write_delay);
            self.spend(bus, remaining);
        }
    }

    /// Read a byte of Game Pak RAM, after any write still in the buffer.
    #[inline]
    pub(crate) fn read_ram(&mut self, bus: &mut dyn Addressable, offset: u16) -> u8 {
        self.finish_ram_write(bus);
        bus.read(self.ram_location(offset))
    }

    /// Hand a byte to the RAM buffer, after the write before it has
    /// landed; it reaches Game Pak RAM a bus access later.
    #[inline]
    pub(crate) fn queue_ram_write(&mut self, bus: &mut dyn Addressable, offset: u16, value: u8) {
        self.finish_ram_write(bus);
        self.ram_write_delay = self.memory_clocks() as u8;
        self.ram_write_address = offset;
        self.ram_write_data = value;
    }

    /// Read a word of Game Pak RAM: the byte at `offset`, then its pair
    /// (`offset` with bit 0 flipped) as the high byte.
    pub(crate) fn read_ram_word(&mut self, bus: &mut dyn Addressable, offset: u16) -> u16 {
        let low = self.read_ram(bus, offset);
        let high = self.read_ram(bus, offset ^ 1);
        u16::from_le_bytes([low, high])
    }

    /// Store a word into Game Pak RAM through the buffer: the low byte at
    /// `offset`, then the high one at its pair.
    pub(crate) fn write_ram_word(&mut self, bus: &mut dyn Addressable, offset: u16, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.queue_ram_write(bus, offset, low);
        self.queue_ram_write(bus, offset ^ 1, high);
    }

    /// Forget every line of the instruction cache.
    #[inline]
    pub(crate) fn flush_cache(&mut self) {
        self.cache.valid = [false; 32];
    }

    /// Fetch a byte of code from `address` in the program bank: through
    /// the cache when the address is one of the 512 from CBR on, loading
    /// its line first if need be; from ROM (banks up to `$5F`) or RAM
    /// otherwise.
    pub(crate) fn fetch(&mut self, bus: &mut dyn Addressable, address: u16) -> u8 {
        let offset = address.wrapping_sub(self.cache_base);
        if offset < 512 {
            let line = usize::from(offset >> 4);
            if self.cache.valid[line] {
                let clocks = self.cache_clocks();
                self.spend(bus, clocks);
            } else {
                let first = usize::from(offset & 0xfff0);
                let source = (u32::from(self.program_bank) << 16)
                    + ((u32::from(self.cache_base) + first as u32) & 0xfff0);
                for i in 0..16 {
                    let clocks = self.memory_clocks();
                    self.spend(bus, clocks);
                    self.cache.bytes[first + i] = bus.read(source + i as u32);
                }
                self.cache.valid[line] = true;
            }
            return self.cache.bytes[usize::from(offset)];
        }
        if self.program_bank <= 0x5f {
            self.finish_rom_read(bus);
        } else {
            self.finish_ram_write(bus);
        }
        let clocks = self.memory_clocks();
        self.spend(bus, clocks);
        bus.read((u32::from(self.program_bank) << 16) + u32::from(address))
    }

    /// The opcode to run now, from the pipeline, which then takes the byte
    /// at R15. The program counter does not move: the caller steps it on
    /// after the instruction unless the instruction jumped.
    pub(crate) fn take_opcode(&mut self, bus: &mut dyn Addressable) -> u8 {
        let opcode = self.next_opcode;
        self.next_opcode = self.fetch(bus, self.registers[15]);
        self.jumped = false;
        opcode
    }

    /// An operand byte, from the pipeline: R15 moves on and the pipeline
    /// takes the byte there.
    pub(crate) fn take_operand(&mut self, bus: &mut dyn Addressable) -> u8 {
        let operand = self.next_opcode;
        self.registers[15] = self.registers[15].wrapping_add(1);
        self.next_opcode = self.fetch(bus, self.registers[15]);
        self.jumped = false;
        operand
    }

    /// Set the GSU up to run the instruction at `entry` next, as starting
    /// it there would: the pipeline holds `entry`'s byte and R15 points
    /// past it. For the host and the tests.
    pub fn prime(&mut self, bus: &mut dyn Addressable, entry: u16) {
        self.next_opcode = self.fetch(bus, entry);
        self.registers[15] = entry.wrapping_add(1);
        self.jumped = false;
        self.clocks = 0;
    }
}
