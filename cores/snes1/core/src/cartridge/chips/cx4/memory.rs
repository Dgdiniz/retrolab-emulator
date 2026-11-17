//! The chip's own bus to the cartridge, its memory accesses, the cache
//! loading, and the step that runs one unit of work.
//!
//! The HG51B169 reaches the cartridge on its own bus ([`Cx4Bus`]): the ROM
//! in banks `$00-$3F` and `$80-$BF` at `$8000-$FFFF`, 32 KiB a bank, and the
//! cart RAM (if any) in banks `$70-$77` at `$0000-$7FFF`. Its memory accesses
//! ([`Hg51b::read_memory`]) see its data RAM in the lower half of the banks
//! with bit 22 clear, at no cost, and pay the ROM's or the RAM's wait states
//! on the cart bus. [`Hg51b::step`] does one unit of work: a whole DMA, a
//! whole cache page, or one instruction.

use super::hg51b::Hg51b;

/// A cap on [`Hg51b::run`], so a program that never halts cannot hang it.
const MAX_RUN_STEPS: u64 = 8_000_000;

/// The cartridge as the chip's bus sees it. It borrows the ROM and the cart
/// RAM from the chip; a RAM write sets `ram_dirty`.
pub struct Cx4Bus<'a> {
    /// The cartridge ROM, repeated to fill the address space.
    pub rom: &'a [u8],
    /// The cart RAM, repeated likewise; empty when there is none.
    pub ram: &'a mut Vec<u8>,
    /// Set when the cart RAM changes.
    pub ram_dirty: &'a mut bool,
}

/// Where an address of the chip's bus lands.
enum Region {
    /// The ROM, at this offset.
    Rom(usize),
    /// The cart RAM, at this offset.
    Ram(usize),
    /// Nothing.
    Unmapped,
}

impl Region {
    fn of(address: u32) -> Region {
        let bank = (address >> 16) & 0xFF;
        let offset = address & 0xFFFF;
        if bank & 0x40 == 0 && offset >= 0x8000 {
            Region::Rom((((bank & 0x3F) << 15) | (offset & 0x7FFF)) as usize)
        } else if bank & 0xF8 == 0x70 && offset < 0x8000 {
            Region::Ram((((bank & 0x07) << 15) | offset) as usize)
        } else {
            Region::Unmapped
        }
    }
}

impl Cx4Bus<'_> {
    /// Read a byte; an empty ROM or RAM, or an unmapped address, reads 0.
    pub fn read(&self, address: u32) -> u8 {
        match Region::of(address) {
            Region::Rom(offset) if !self.rom.is_empty() => self.rom[offset % self.rom.len()],
            Region::Ram(offset) if !self.ram.is_empty() => self.ram[offset % self.ram.len()],
            Region::Rom(_) | Region::Ram(_) | Region::Unmapped => 0,
        }
    }

    /// Write a byte; only the cart RAM takes it.
    pub fn write(&mut self, address: u32, value: u8) {
        if let Region::Ram(offset) = Region::of(address) {
            if !self.ram.is_empty() {
                let len = self.ram.len();
                self.ram[offset % len] = value;
                *self.ram_dirty = true;
            }
        }
    }
}

/// What a memory access of the core reaches.
enum Access {
    /// The data RAM, at this index: no wait.
    DataRam(usize),
    /// The cart RAM's banks (`$70-$7F`, lower half): the RAM's wait states.
    CartRam,
    /// The ROM's half of banks `$00-$3F`/`$80-$BF`: the ROM's wait states.
    CartRom,
    /// Nothing, at no cost.
    Nothing,
}

impl Access {
    fn of(address: u32) -> Access {
        let bank = (address >> 16) & 0xFF;
        let offset = address & 0xFFFF;
        let low_half = offset < 0x8000;
        if bank & 0x40 == 0 && low_half && offset & 0x0C00 != 0x0C00 {
            Access::DataRam((offset & 0x0FFF) as usize)
        } else if bank & 0xF0 == 0x70 && low_half {
            Access::CartRam
        } else if bank & 0x40 == 0 && !low_half {
            Access::CartRom
        } else {
            Access::Nothing
        }
    }
}

impl Hg51b {
    /// A read by the core, with the wait states it costs.
    pub fn read_memory(&mut self, address: u32, bus: &mut Cx4Bus) -> u8 {
        match Access::of(address) {
            Access::DataRam(index) => self.data_ram[index],
            Access::CartRam => {
                self.spend(u32::from(self.io.ram_wait), bus);
                bus.read(address)
            }
            Access::CartRom => {
                self.spend(u32::from(self.io.rom_wait), bus);
                bus.read(address)
            }
            Access::Nothing => 0,
        }
    }

    /// A write by the core, with the wait states it costs (a write to the
    /// ROM costs them and changes nothing).
    pub fn write_memory(&mut self, address: u32, value: u8, bus: &mut Cx4Bus) {
        match Access::of(address) {
            Access::DataRam(index) => self.data_ram[index] = value,
            Access::CartRam => {
                self.spend(u32::from(self.io.ram_wait), bus);
                bus.write(address, value);
            }
            Access::CartRom => {
                self.spend(u32::from(self.io.rom_wait), bus);
                bus.write(address, value);
            }
            Access::Nothing => {}
        }
    }

    /// Fill cache page `slot` with program page `page`: 256 little-endian
    /// words from the program offset plus 512 bytes a page, each costing its
    /// two reads and one clock.
    pub fn load_cache_page(&mut self, slot: u8, page: u16, bus: &mut Cx4Bus) {
        let slot = usize::from(slot);
        let start = self.io.program_offset.wrapping_add(u32::from(page) << 9);
        self.io.cache_loading = true;
        for word in 0..256u32 {
            let address = start.wrapping_add(2 * word);
            let low = self.read_memory(address, bus);
            let high = self.read_memory(address.wrapping_add(1), bus);
            self.cache[slot].words[word as usize] = u16::from_le_bytes([low, high]);
            self.spend(1, bus);
        }
        self.cache[slot].page = page;
        self.io.cache_loading = false;
    }

    /// Start the program at the S-CPU's program page and entry point, in
    /// cache page 0, if the core is halted.
    pub fn start_program(&mut self) {
        if self.halted {
            self.pc = (u32::from(self.io.program_page) << 8) + u32::from(self.io.entry_pc);
            self.halted = false;
            self.active_page = 0;
        }
    }

    /// Ask for the DMA the S-CPU set up, if the core is halted.
    pub fn request_dma(&mut self) {
        if self.halted {
            self.io.dma_pending = true;
        }
    }

    /// Ask for cache page `value & 1` to be loaded with the program page,
    /// if the core is halted.
    pub fn request_preload(&mut self, value: u8) {
        if self.halted {
            self.io.preload_page = value & 1;
            self.io.cache_loading = true;
        }
    }

    /// One unit of work: the DMA asked for, else the preload asked for, else
    /// one instruction, else one idle clock. Going from busy to idle raises
    /// the interrupt to the S-CPU.
    pub fn step(&mut self, bus: &mut Cx4Bus) {
        let was_busy = self.busy();
        if self.io.dma_pending {
            self.run_dma(bus);
        } else if self.io.cache_loading {
            let (slot, page) = (self.io.preload_page, self.io.program_page);
            self.load_cache_page(slot, page, bus);
        } else if !self.halted {
            self.run_instruction(bus);
        } else {
            self.spend(1, bus);
        }
        if was_busy && !self.busy() {
            self.irq_pending = true;
        }
    }

    /// The DMA: each byte read from the source and written to the target,
    /// one clock apiece on top of the accesses' wait states.
    fn run_dma(&mut self, bus: &mut Cx4Bus) {
        let (source, target) = (self.io.dma_source, self.io.dma_target);
        for n in 0..u32::from(self.io.dma_length) {
            let value = self.read_memory(source.wrapping_add(n), bus);
            self.write_memory(target.wrapping_add(n), value, bus);
            self.spend(1, bus);
        }
        self.io.dma_pending = false;
    }

    /// Fetch the instruction at the program counter, loading its page into
    /// the active cache page if that page holds another, step the program
    /// counter past it, and run it.
    fn run_instruction(&mut self, bus: &mut Cx4Bus) {
        let wanted = (self.pc >> 8) as u16;
        let slot = self.active_page;
        if self.cache[usize::from(slot)].page != wanted {
            self.load_cache_page(slot, wanted, bus);
        }
        self.opcode = self.cache[usize::from(slot)].words[(self.pc & 0xFF) as usize];
        self.advance_pc();
        self.execute(self.opcode, bus);
    }

    /// Step until the chip is idle, the pending DMA or preload, then the
    /// program to its halt, or one step past [`MAX_RUN_STEPS`] has run.
    pub fn run(&mut self, bus: &mut Cx4Bus) {
        for _ in 0..=MAX_RUN_STEPS {
            if !self.busy() {
                return;
            }
            self.step(bus);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a 16-bit `word` little-endian at byte `offset` in `rom`.
    fn poke_word(rom: &mut [u8], offset: usize, word: u16) {
        rom[offset] = word as u8;
        rom[offset + 1] = (word >> 8) as u8;
    }

    // ---- the chip's bus to the cartridge -------------------------------------

    #[test]
    fn cx4_bus_reads_the_rom_and_its_mirror() {
        let mut rom = vec![0u8; 0x1_0000]; // two 32 KiB banks
        rom[0] = 0x11;
        rom[0x7FFF] = 0x22;
        rom[0x8000] = 0x33; // bank 1, offset 0
        let mut ram = Vec::new();
        let mut dirty = false;
        let bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        assert_eq!(bus.read(0x00_8000), 0x11, "bank 0 offset 0");
        assert_eq!(bus.read(0x00_FFFF), 0x22, "bank 0 offset 0x7fff");
        assert_eq!(bus.read(0x01_8000), 0x33, "bank 1 offset 0");
        assert_eq!(bus.read(0x80_8000), 0x11, "$80-bf mirrors $00-3f");
        // The low half of a bank ($0000-7FFF) is not ROM on the Cx4 bus.
        assert_eq!(bus.read(0x00_0000), 0);
    }

    #[test]
    fn cx4_bus_repeats_a_small_rom() {
        let mut rom = vec![0u8; 0x8000]; // a single 32 KiB bank
        rom[0] = 0xAB;
        let mut ram = Vec::new();
        let mut dirty = false;
        let bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        // Bank 1 offset 0 -> linear 0x8000 -> folds modulo 0x8000 -> rom[0].
        assert_eq!(bus.read(0x01_8000), 0xAB);
    }

    #[test]
    fn cx4_bus_cart_ram_write_sets_dirty() {
        let rom = vec![0u8; 0x8000];
        let mut ram = vec![0u8; 0x8000];
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        bus.write(0x70_0000, 0xCD);
        assert_eq!(bus.read(0x70_0000), 0xCD);
        assert!(
            *bus.ram_dirty,
            "a cart-RAM write flips the battery-dirty flag"
        );
        // ROM is read-only: a write to the ROM window is dropped.
        *bus.ram_dirty = false;
        bus.write(0x00_8000, 0x55);
        assert_eq!(bus.read(0x00_8000), 0x00);
        assert!(!*bus.ram_dirty);
    }

    #[test]
    fn cx4_bus_cart_ram_banks() {
        let rom = vec![0u8; 0x8000];
        let mut ram = vec![0u8; 0x1_0000]; // two banks of cart RAM
        ram[0x8000] = 0x77;
        let mut dirty = false;
        let bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        // $71:0000 -> bank 1 -> linear 0x8000.
        assert_eq!(bus.read(0x71_0000), 0x77);
    }

    // ---- the core's memory accesses and their wait states --------------------

    #[test]
    fn read_memory_waits_by_region() {
        let mut rom = vec![0u8; 0x8000];
        rom[0] = 0x5A;
        let mut ram = vec![0u8; 0x8000];
        ram[0] = 0xA5;
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.rom_wait = 3;
        c.io.ram_wait = 2;
        c.data_ram[0x10] = 0x99;

        // Data RAM: no wait.
        c.cycles = 0;
        assert_eq!(c.read_memory(0x10, &mut bus), 0x99);
        assert_eq!(c.cycles, 0, "the data RAM has no wait");

        // Cart ROM: the ROM's wait states.
        c.cycles = 0;
        assert_eq!(c.read_memory(0x00_8000, &mut bus), 0x5A);
        assert_eq!(c.cycles, 3);

        // Cart RAM: the RAM's wait states.
        c.cycles = 0;
        assert_eq!(c.read_memory(0x70_0000, &mut bus), 0xA5);
        assert_eq!(c.cycles, 2);
    }

    #[test]
    fn write_memory_waits_by_region() {
        let rom = vec![0u8; 0x8000];
        let mut ram = vec![0u8; 0x8000];
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.ram_wait = 2;

        // Data RAM: no wait.
        c.cycles = 0;
        c.write_memory(0x20, 0x42, &mut bus);
        assert_eq!(c.data_ram[0x20], 0x42);
        assert_eq!(c.cycles, 0);

        // Cart RAM: the RAM's wait states; the byte lands and sets dirty.
        c.cycles = 0;
        c.write_memory(0x70_0001, 0x84, &mut bus);
        assert_eq!(c.cycles, 2);
        assert_eq!(ram[1], 0x84);
        assert!(dirty);
    }

    // ---- the instruction cache ------------------------------------------------

    #[test]
    fn load_cache_page_fills_256_words_and_counts_clocks() {
        let mut rom = vec![0u8; 0x8000];
        // The first 256 words, little-endian, each one different.
        for i in 0..256usize {
            poke_word(&mut rom, i * 2, 0x0100u16.wrapping_mul(i as u16) ^ 0x00AB);
        }
        let mut ram = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.rom_wait = 1;
        c.io.program_offset = 0x00_8000; // bank 0's ROM half -> ROM offset 0
        c.cycles = 0;

        c.load_cache_page(0, 0, &mut bus);

        for i in 0..256usize {
            let expected = 0x0100u16.wrapping_mul(i as u16) ^ 0x00AB;
            assert_eq!(c.cache[0].words[i], expected, "cache word {i}");
        }
        assert_eq!(c.cache[0].page, 0, "the page is recorded");
        assert!(!c.io.cache_loading, "the load is over");
        // A word: two ROM reads (one wait each) and a clock = 3 -> 256 * 3.
        assert_eq!(c.cycles, 256 * 3);
    }

    #[test]
    fn a_loaded_page_is_kept_by_select_page_for_pc() {
        let mut rom = vec![0u8; 0x8000];
        poke_word(&mut rom, 0, 0x6400); // any word
        let mut ram = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.halted = false;
        c.io.program_offset = 0x00_8000;
        c.load_cache_page(0, 0, &mut bus);
        c.pc = 0x00_0010; // program page 0, held by cache page 0
        c.select_page_for_pc();
        assert_eq!(c.active_page, 0, "still on the loaded page");
        assert!(!c.halted);
    }

    // ---- starting the program -------------------------------------------------

    #[test]
    fn start_program_sets_the_pc_and_runs() {
        let mut c = Hg51b::new();
        c.io.program_page = 0x0002;
        c.io.entry_pc = 0x10;
        assert!(c.halted);
        c.start_program();
        assert_eq!(c.pc, 0x02 * 256 + 0x10);
        assert!(!c.halted);
        assert_eq!(c.active_page, 0);

        // A second start while it runs changes nothing.
        c.io.program_page = 0x0005;
        c.start_program();
        assert_eq!(c.pc, 0x0210, "start is ignored while running");
    }

    // ---- DMA --------------------------------------------------------------------

    #[test]
    fn dma_copies_rom_to_data_ram_with_clocks() {
        let mut rom = vec![0u8; 0x8000];
        rom[0] = 0xAA;
        rom[1] = 0xBB;
        rom[2] = 0xCC;
        rom[3] = 0xDD;
        let mut ram = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.rom_wait = 1;
        c.io.dma_source = 0x00_8000; // ROM offset 0
        c.io.dma_target = 0x00_0000; // the data RAM
        c.io.dma_length = 4;
        c.request_dma();
        assert!(c.io.dma_pending, "the DMA is asked for while halted");
        c.cycles = 0;

        c.step(&mut bus);

        assert_eq!(&c.data_ram[0..4], &[0xAA, 0xBB, 0xCC, 0xDD]);
        assert!(!c.io.dma_pending, "the DMA is done");
        // A byte: a ROM read (one wait), a data RAM write (none) and a clock.
        assert_eq!(c.cycles, 4 * 2);
        assert!(!c.busy(), "idle again after DMA");
        assert!(c.irq_pending, "going idle raised the interrupt");
    }

    #[test]
    fn dma_is_inert_unless_halted() {
        let mut c = Hg51b::new();
        c.halted = false; // running
        c.request_dma();
        assert!(!c.io.dma_pending, "a running core ignores the request");
    }

    // ---- the cache preload ------------------------------------------------------

    #[test]
    fn preload_populates_a_page() {
        let mut rom = vec![0u8; 0x8000];
        poke_word(&mut rom, 0, 0x1234);
        poke_word(&mut rom, 2, 0x5678);
        let mut ram = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.program_offset = 0x00_8000;
        c.io.program_page = 0;
        c.request_preload(0);
        assert!(c.io.cache_loading, "the preload is asked for while halted");

        c.step(&mut bus);

        assert_eq!(c.cache[0].words[0], 0x1234);
        assert_eq!(c.cache[0].words[1], 0x5678);
        assert_eq!(c.cache[0].page, 0);
        assert!(!c.io.cache_loading, "the load is over");
    }

    // ---- cart-bus transfers -----------------------------------------------------

    #[test]
    fn a_bus_read_lands_in_the_bus_data_latch() {
        let mut rom = vec![0u8; 0x8000];
        rom[0] = 0x5A;
        let mut ram = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.rom_wait = 3;
        c.bus_address = 0x00_8000;
        c.read_register(0x2E); // start a read: the ROM's wait + 1 = 4 clocks
        assert_eq!(c.transfer.remaining, 4);
        c.spend(4, &mut bus);
        assert_eq!(c.bus_data, 0x5A, "the read landed in the bus data latch");
        assert_eq!(c.last_value, 0x5A);
        assert_eq!(c.transfer.remaining, 0);
    }

    #[test]
    fn a_bus_write_reaches_the_cart() {
        let rom = vec![0u8; 0x8000];
        let mut ram = vec![0u8; 0x8000];
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.ram_wait = 3;
        c.bus_address = 0x70_0000;
        c.write_register(0x2F, 0x99); // start a write: the RAM's wait + 1 clocks
        c.spend(4, &mut bus);
        assert_eq!(ram[0], 0x99, "the write reached the cart RAM");
        assert!(dirty);
        assert_eq!(c.transfer.remaining, 0);
    }

    #[test]
    fn loop_waits_out_the_transfer() {
        let mut rom = vec![0u8; 0x8000];
        rom[0] = 0x3C;
        let mut ram = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.rom_wait = 3;
        c.bus_address = 0x00_8000;
        c.read_register(0x2E); // 4 clocks
                               // `loop` (0x1C00) spends the 3 clocks left past its own, then its own.
        c.execute(0x1C00, &mut bus);
        assert_eq!(c.bus_data, 0x3C, "loop waited out the transfer");
        assert_eq!(c.transfer.remaining, 0);
    }

    // ---- end to end: run a real program from ROM ----------------------------

    #[test]
    fn end_to_end_program_writes_data_ram_then_halts() {
        // A tiny program: 0x12 into data_ram[0], 0x34 into data_ram[1], then
        // halt; little-endian words at the start of the ROM.
        let program: &[u16] = &[
            0x6412, // ld a,#0x12
            0xE00C, // st ramdata,a   (register $0C is the RAM data latch)
            0xEC00, // wrraml [#0x00]
            0x6434, // ld a,#0x34
            0xE00C, // st ramdata,a
            0xEC01, // wrraml [#0x01]
            0xFC00, // halt
        ];
        let mut rom = vec![0u8; 0x8000];
        for (i, &word) in program.iter().enumerate() {
            poke_word(&mut rom, i * 2, word);
        }
        let mut ram = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.program_offset = 0x00_8000;
        c.io.program_page = 0;
        c.io.entry_pc = 0;
        c.start_program();
        assert!(!c.halted, "started");

        c.run(&mut bus);

        assert!(c.halted, "ran to the halt instruction");
        assert_eq!(c.data_ram[0], 0x12);
        assert_eq!(c.data_ram[1], 0x34);
        // Program page 0 was loaded into cache page 0.
        assert_eq!(c.cache[0].page, 0);
    }

    #[test]
    fn end_to_end_alu_program_computes_into_data_ram() {
        // Compute (7 + 5) and store the low byte to data_ram[4].
        let program: &[u16] = &[
            0x6407, // ld a,#0x07
            0x8405, // add a,#0x05      -> a = 0x0c
            0xE00C, // st ramdata,a     -> the RAM data latch = 0x0c
            0xEC04, // wrraml [#0x04]   -> data_ram[4] = 0x0c
            0xFC00, // halt
        ];
        let mut rom = vec![0u8; 0x8000];
        for (i, &word) in program.iter().enumerate() {
            poke_word(&mut rom, i * 2, word);
        }
        let mut ram = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &rom,
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.io.program_offset = 0x00_8000;
        c.start_program();
        c.run(&mut bus);
        assert_eq!(c.data_ram[4], 0x0C);
    }
}
