//! Cx4 (Hitachi HG51B169) cartridge coprocessor.
//!
//! The "Cx4" (Capcom's commercial label) is a Hitachi HG51B169 DSP running at
//! 20 MHz, used by exactly two cartridges, Mega Man X2 and Mega Man X3, to
//! accelerate 3D wireframe maths (the spinning "X" intro, vector effects). It
//! does not draw pixels: it transforms vertices into its internal data RAM,
//! which the S-CPU then reads back to render.
//!
//! Like the other coprocessors (DSP-1, S-DD1, SPC7110, SA-1, Super FX), the
//! Cx4 is an [`impl Coprocessor`](retro_core::cartridge::Coprocessor) homed in
//! a `ComposableCartridge` and built by the console factory. It is the closest
//! sibling to the Super FX, but the HG51B169 core is small enough (45
//! instruction families, no delay slot, no plot engine) to live in-module
//! here rather than in a dedicated `cpus/*` crate.
//!
//! ## One chip, three windows, one `chip_id`
//!
//! A single [`Cx4Chip`] answers on three disjoint bus windows, all routed here
//! as [`MapTarget::ChipMmio`](retro_core::cartridge::MapTarget) from
//! [`super::presets::cx4_preset`] and split apart by the `base` sentinel each
//! slot carries:
//!
//! - **MMIO + data RAM** (`base = CX4_MMIO_BASE`): the `$6000-7FFF` window in
//!   both system-bank halves. The slot folds the source bank away (stride `0`),
//!   so an access arrives as `CX4_MMIO_BASE + (addr & 0x1FFF)`; the chip
//!   recovers the `$6000-7FFF` SNES offset and decodes the internal data RAM
//!   (`$6000-6BFF`/`$7000-7BFF`) from the register file (`$7F40-7FFF`).
//! - **ROM** (`base = CX4_ROM_BASE`): the LoROM `$8000-FFFF` half-banks. The
//!   read is chip-routed so bus contention can interpose while the HG51B169
//!   owns the cart bus; while the chip is idle it returns the real ROM byte, so
//!   this is transparent for a booting game.
//! - **Cart RAM** (`base = CX4_RAM_BASE`, only when a manifest declares it):
//!   the `$70-77:0000-7FFF` window. The real MMX2/X3 carts have no SRAM (they
//!   save by password), but RetroLab is a lab for ROMs: a manifest that sets
//!   `sram_size > 0` gets a battery-backed `.srm`, mirroring the SPC7110/SA-1
//!   size-driven battery model.
//!
//! ## Live timing
//!
//! The HG51B169 runs on the S-CPU's clock through lazy catch-up. [`Cx4Chip`]
//! holds an [`IntegerRateConverter`] from the ~21.477 MHz master clock to the
//! chip's 20 MHz, and every S-CPU access to the window first calls
//! [`catch_up`](Cx4Chip::catch_up), which steps the core (`step`) up to the
//! chip cycle that access lands on, the same rate-converted model the DSP-1
//! and SPC7110 use. The S-CPU asks for work through the chip's registers
//! (`$7F47` a DMA, `$7F48` a preload, `$7F4F` a program run), the catch-up
//! does it, and going from busy to idle latches [`has_irq`](Cx4Chip::has_irq)
//! so the console's IRQ line fires. While the chip uses the cart bus, the
//! S-CPU's ROM reads give 0 except at the reset and interrupt vectors.
//!
//! The core is written in this tree's own way: the instruction set as a
//! table of families, the register window decoded into named ports, the
//! arithmetic grouped by kind. Its behaviour is the one bsnes-plus's Cx4
//! has, checked against it; where it differs, the code says so.

pub mod data;
pub mod hg51b;
pub mod instructions;
pub mod memory;
mod ports;
pub mod registers;

use std::sync::Arc;

use hg51b::{BusTransfer, Hg51b};
use memory::Cx4Bus;
use retro_core::audio::IntegerRateConverter;
use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};
use retro_core::emulator::StepResult;
use retro_core::types::{CpuState, Instruction};

use crate::debugger::{cpu_state_hg51b, TraceEntryBinaryCx4};
use crate::savestate::{ConverterState, CoprocessorState, Cx4State};

pub use crate::cartridge::chip_ids::{CX4_CHIP_ID, CX4_MMIO_BASE, CX4_RAM_BASE, CX4_ROM_BASE};

/// The HG51B169's own clock (20 MHz), independent of and slower than the SNES
/// master clock (~21.477 MHz NTSC). The lazy [`catch_up`](Cx4Chip::catch_up)
/// converts master cycles into chip cycles through an [`IntegerRateConverter`],
/// the same rate-converted model the DSP-1/SPC7110 use (Super FX, by contrast,
/// runs 1:1 on the master clock).
pub const CX4_CLOCK_HZ: u64 = 20_000_000;

/// NTSC master clock, the converter's denominator until [`Cx4Chip::reset`]
/// rebinds it to the console's actual region clock.
const DEFAULT_MASTER_CLOCK_HZ: u64 = 21_477_270;

/// The Cx4 coprocessor: cart ROM, an optional battery-backed cart RAM, and the
/// HG51B169 execution core (which owns the internal data RAM, the register-file
/// latches, the instruction cache, and the DSP registers).
pub struct Cx4Chip {
    /// Cartridge ROM, the HG51B169's program/DMA source and the S-CPU's ROM
    /// window (through [`CX4_ROM_BASE`], for bus contention).
    rom: Arc<[u8]>,
    /// Cart RAM, owned by the chip (mirrored modulo its length). Empty on the
    /// real MMX2/X3 carts; a manifest may declare it for homebrew.
    ram: Vec<u8>,
    /// The HG51B169 execution core (registers, cache, MMIO latches, data RAM).
    core: Hg51b,
    /// Master-clock -> 20 MHz chip-clock converter. The lazy catch-up steps the
    /// core forward to the cumulative chip cycle each S-CPU access lands on.
    sync: IntegerRateConverter,
    /// Whether cart RAM is battery-backed (a manifest declared `sram_size > 0`).
    has_battery: bool,
    /// Whether cart RAM changed since the last `.srm` flush.
    ram_dirty: bool,
    /// Last byte the S-CPU put on / took off the chip bus, returned as the
    /// open-bus value for reads that hit neither a register nor the data RAM.
    last_mdr: u8,
    /// S-CPU accesses (reads + writes) to the chip since reset.
    accesses: u64,
    /// Whether the debugger has armed HG51B169 instruction tracing (cpu 2).
    trace_enabled: bool,
    /// Trace records captured during `catch_up` since the last drain, each a
    /// [`TRACE_ENTRY_SIZE_CX4`](crate::debugger::TRACE_ENTRY_SIZE_CX4)-byte
    /// [`TraceEntryBinaryCx4`](crate::debugger::TraceEntryBinaryCx4).
    trace_buf: Vec<u8>,
    /// Debugger metadata (type, name, features).
    info: ExpansionChipInfo,
}

impl Cx4Chip {
    /// Build a Cx4 over `rom`. `ram_size` is the cart RAM size in bytes (0 for
    /// none); `has_battery` marks that RAM as `.srm`-persistent. `master_clock_hz`
    /// seeds the rate converter; [`reset`](Self::reset) re-anchors it to the
    /// running console's region clock.
    pub fn new(rom: Arc<[u8]>, ram_size: usize, has_battery: bool, master_clock_hz: u64) -> Self {
        let master_clock_hz = if master_clock_hz == 0 {
            DEFAULT_MASTER_CLOCK_HZ
        } else {
            master_clock_hz
        };
        Self {
            rom,
            ram: vec![0u8; ram_size],
            core: Hg51b::new(),
            sync: IntegerRateConverter::new(CX4_CLOCK_HZ, master_clock_hz),
            has_battery,
            ram_dirty: false,
            last_mdr: 0,
            accesses: 0,
            trace_enabled: false,
            trace_buf: Vec::new(),
            info: ExpansionChipInfo {
                name: "Cx4".to_string(),
                chip_type: ExpansionChipType::CX4,
                description: "Hitachi HG51B169 DSP (Mega Man X2/X3)".to_string(),
                features: vec![
                    "24-bit DSP".to_string(),
                    "3D wireframe maths".to_string(),
                    "internal data ROM + RAM".to_string(),
                ],
            },
        }
    }

    /// The S-CPU reads cart ROM through the chip. While the HG51B169 uses
    /// the cart bus and is not suspended, the S-CPU sees 0, except at the
    /// reset and interrupt vectors, `$00:FFC0-FFFF` (offsets
    /// `0x7FC0..=0x7FFF`), which stay readable so it can still take an
    /// interrupt. Idle or suspended, the chip passes the real byte. (bsnes-plus
    /// answers the vector window from the chip's registers; this core gives
    /// the ROM's own vector bytes, safer for a booting CPU.)
    fn rom_read(&self, rom_offset: u32) -> u8 {
        if self.rom.is_empty() {
            return 0;
        }
        let real = self.rom[(rom_offset as usize) % self.rom.len()];
        // The chip contends the cart bus only while it is actively using it and
        // not suspended; the vector window survives even then.
        let contended = self.core.bus_in_use() && !self.core.io.suspended;
        if contended && !(0x7FC0..=0x7FFF).contains(&rom_offset) {
            0
        } else {
            real
        }
    }

    /// Read cart RAM (mirrored modulo its size). Empty RAM reads 0.
    fn cart_ram_read(&self, ram_offset: u32) -> u8 {
        if self.ram.is_empty() {
            return 0;
        }
        self.ram[(ram_offset as usize) % self.ram.len()]
    }

    /// Write cart RAM (mirrored modulo its size). Empty RAM drops the write.
    fn cart_ram_write(&mut self, ram_offset: u32, value: u8) {
        if self.ram.is_empty() {
            return;
        }
        let len = self.ram.len();
        self.ram[(ram_offset as usize) % len] = value;
        self.ram_dirty = true;
    }

    /// Single-step the HG51B169 by one unit of work (an instruction, or one
    /// pending DMA / cache-page load) against the real cart bus, the debugger
    /// step model. A halted core with nothing armed has nothing to run and
    /// reports `instruction_executed: false`, mirroring
    /// [`SuperFxChip::step_one`](crate::cartridge::SuperFxChip). The clocks the
    /// step spends land in `core.cycles`, so a later [`catch_up`](Self::catch_up)
    /// self-waits until the S-CPU's master clock reaches them.
    fn step_one(&mut self) -> StepResult {
        if !self.core.busy() {
            return StepResult {
                cycles: 0,
                breakpoint_hit: false,
                instruction_executed: false,
            };
        }
        let before = self.core.cycles;
        {
            let mut bus = Cx4Bus {
                rom: &self.rom,
                ram: &mut self.ram,
                ram_dirty: &mut self.ram_dirty,
            };
            self.core.step(&mut bus);
        }
        StepResult {
            cycles: (self.core.cycles - before) as u32,
            breakpoint_hit: false,
            instruction_executed: true,
        }
    }

    /// Snapshot the HG51B169 state *before* the instruction at `pc` executes,
    /// as a serialized [`TraceEntryBinaryCx4`]. Takes the core + ROM as disjoint
    /// borrows so it can be called inside `catch_up`'s step loop while the
    /// mutable [`Cx4Bus`] borrows `ram`/`ram_dirty`.
    fn capture_trace_entry(
        core: &Hg51b,
        rom: &[u8],
    ) -> [u8; crate::debugger::TRACE_ENTRY_SIZE_CX4] {
        let pc = core.pc;
        let opcode = program_word(core, rom, pc);
        let f = &core.flags;
        let flags = u8::from(f.negative) << 3
            | u8::from(f.zero) << 2
            | u8::from(f.overflow) << 1
            | u8::from(f.carry);
        TraceEntryBinaryCx4 {
            cycles: core.cycles,
            pc,
            opcode,
            p: core.page,
            flags,
            a: core.accumulator,
            acch: core.product_high,
            accl: core.product_low,
            gpr: core.registers,
        }
        .to_bytes()
    }
}

/// The program word at program address `pc`, for the debugger: from the
/// cache page that holds its page, else from where
/// [`Hg51b::load_cache_page`] would load it, the program offset plus 512
/// bytes a page, read through the chip's bus with no cart RAM, so nothing
/// is changed.
fn program_word(core: &Hg51b, rom: &[u8], pc: u32) -> u16 {
    let page = (pc >> 8) as u16;
    let word = (pc & 0xFF) as usize;
    if let Some(cached) = core.cache.iter().find(|cached| cached.page == page) {
        return cached.words[word];
    }
    let mut no_ram = Vec::new();
    let mut dirty = false;
    let bus = Cx4Bus {
        rom,
        ram: &mut no_ram,
        ram_dirty: &mut dirty,
    };
    let address = core
        .io
        .program_offset
        .wrapping_add(u32::from(page) << 9)
        .wrapping_add(2 * word as u32);
    u16::from_le_bytes([bus.read(address), bus.read(address.wrapping_add(1))])
}

impl Coprocessor for Cx4Chip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, master_cycle: u64) -> u8 {
        // Run the HG51B169 up to the S-CPU's cycle first, so the status/busy/
        // data RAM this access observes reflects everything the chip did before.
        self.catch_up(master_cycle);
        self.accesses += 1;
        let value = if offset >= CX4_ROM_BASE {
            self.rom_read(offset - CX4_ROM_BASE)
        } else if offset >= CX4_RAM_BASE {
            self.cart_ram_read(offset - CX4_RAM_BASE)
        } else {
            let addr = 0x6000 | (offset.wrapping_sub(CX4_MMIO_BASE) & 0x1FFF) as u16;
            self.core.read_window(addr, self.last_mdr)
        };
        self.last_mdr = value;
        value
    }

    fn write(&mut self, offset: u32, value: u8, master_cycle: u64) {
        self.catch_up(master_cycle);
        self.accesses += 1;
        self.last_mdr = value;
        if offset >= CX4_ROM_BASE {
            // Cart ROM is read-only.
        } else if offset >= CX4_RAM_BASE {
            self.cart_ram_write(offset - CX4_RAM_BASE, value);
        } else {
            let addr = 0x6000 | (offset.wrapping_sub(CX4_MMIO_BASE) & 0x1FFF) as u16;
            self.core.write_window(addr, value);
        }
    }

    fn catch_up(&mut self, master_cycle: u64) {
        if master_cycle <= self.sync.reference_cycles() {
            return;
        }
        // Cumulative 20 MHz chip cycles due by `master_cycle`. The core's
        // `cycles` accumulator lives in the same domain, so it is the step
        // budget: run whole instructions / DMA / cache-loads until the chip has
        // consumed the budget or gone idle.
        let target = self.sync.cycles_to_samples(master_cycle);
        {
            let mut bus = Cx4Bus {
                rom: &self.rom,
                ram: &mut self.ram,
                ram_dirty: &mut self.ram_dirty,
            };
            while self.core.cycles < target && self.core.busy() {
                // Trace the instruction about to execute (not DMA/cache/idle
                // cycles). `self.core` and `self.rom` are disjoint from the bus'
                // `ram`/`ram_dirty` borrows, so the capture reads them freely.
                if self.trace_enabled
                    && !self.core.io.dma_pending
                    && !self.core.io.cache_loading
                    && !self.core.halted
                {
                    let entry = Self::capture_trace_entry(&self.core, &self.rom);
                    self.trace_buf.extend_from_slice(&entry);
                }
                self.core.step(&mut bus);
            }
        }
        // An idle chip absorbs the remaining budget (it did nothing during those
        // master cycles). An instruction/DMA that overshot `target` leaves
        // `cycles > target`, so the next catch-up simply waits for the master
        // clock to reach it, the same self-throttle the Super FX anchor uses.
        if self.core.cycles < target {
            self.core.cycles = target;
        }
        self.sync.advance(master_cycle, target);
    }

    fn has_irq(&self) -> bool {
        self.core.irq_pending && !self.core.io.irq_masked
    }

    fn reset(&mut self, master_clock_hz: u64) {
        // Return the core to its power-on state (halted -> status idle) and
        // re-anchor the rate converter to the running console's region clock.
        // Cart RAM (battery-backed) lives on the chip and survives a soft reset.
        self.core.reset();
        let master_clock_hz = if master_clock_hz == 0 {
            DEFAULT_MASTER_CLOCK_HZ
        } else {
            master_clock_hz
        };
        self.sync = IntegerRateConverter::new(CX4_CLOCK_HZ, master_clock_hz);
        self.last_mdr = 0;
        self.accesses = 0;
    }

    /// Volatile savestate: the whole HG51B169 core (registers, call stack,
    /// the S-CPU's settings), both cache pages and the data RAM, plus the
    /// rate converter's anchor. Cart ROM and the data ROM never change and
    /// cart RAM is the battery, so none of them is captured. The DTO is the
    /// savestate format, so its fields keep their own names, not the core's.
    fn serialize(&self) -> Vec<u8> {
        let core = &self.core;
        let io = &core.io;
        let mut cache_data = Vec::with_capacity(512);
        cache_data.extend_from_slice(&core.cache[0].words);
        cache_data.extend_from_slice(&core.cache[1].words);
        let state = CoprocessorState::Cx4(Cx4State {
            pc: core.pc,
            p: core.page,
            n: core.flags.negative,
            z: core.flags.zero,
            v: core.flags.overflow,
            c: core.flags.carry,
            a: core.accumulator,
            acch: core.product_high,
            accl: core.product_low,
            busdata: core.bus_data,
            romdata: core.rom_data,
            ramdata: core.ram_data,
            busaddr: core.bus_address,
            ramaddr: core.ram_base,
            gpr: core.registers,
            mdr: core.last_value,
            stack: core.call_stack,
            halt: core.halted,
            cache_page: core.active_page,
            rwbusaddr: core.transfer.address,
            rwbustime: core.transfer.remaining,
            writebus: core.transfer.write,
            writebusdata: core.transfer.data,
            dma_source: io.dma_source,
            dma_length: io.dma_length,
            dma_target: io.dma_target,
            program_offset: io.program_offset,
            page_number: io.program_page,
            program_counter: io.entry_pc,
            rom_speed: io.rom_wait,
            ram_speed: io.ram_wait,
            irq_disable: io.irq_masked,
            irq_pending: core.irq_pending,
            dma: io.dma_pending,
            cache_loading: io.cache_loading,
            cache_preload: io.preload_page,
            suspend: io.suspended,
            suspend_cycles: io.suspend_remaining,
            vectors: io.vectors.to_vec(),
            cache_lock: [core.cache[0].locked, core.cache[1].locked],
            cache_page_number: [core.cache[0].page, core.cache[1].page],
            cache_data,
            data_ram: core.data_ram.to_vec(),
            sync: ConverterState {
                reference_cycles: self.sync.reference_cycles(),
                samples_at_reference: self.sync.samples_at_reference(),
                remainder: self.sync.remainder(),
            },
            cx4_cycles: core.cycles,
            accesses: self.accesses,
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], master_clock_hz: u64) {
        // A blob that does not decode to a Cx4 state is ignored and the chip
        // left as it is.
        let Ok(CoprocessorState::Cx4(state)) = bincode::deserialize::<CoprocessorState>(data)
        else {
            return;
        };
        let core = &mut self.core;
        core.pc = state.pc;
        core.page = state.p;
        core.flags.negative = state.n;
        core.flags.zero = state.z;
        core.flags.overflow = state.v;
        core.flags.carry = state.c;
        core.accumulator = state.a;
        core.product_high = state.acch;
        core.product_low = state.accl;
        core.bus_data = state.busdata;
        core.rom_data = state.romdata;
        core.ram_data = state.ramdata;
        core.bus_address = state.busaddr;
        core.ram_base = state.ramaddr;
        core.registers = state.gpr;
        core.last_value = state.mdr;
        core.halted = state.halt;
        core.active_page = state.cache_page;
        core.irq_pending = state.irq_pending;
        core.transfer = BusTransfer {
            address: state.rwbusaddr,
            remaining: state.rwbustime,
            write: state.writebus,
            data: state.writebusdata,
        };
        core.call_stack = state.stack;

        let io = &mut core.io;
        io.dma_source = state.dma_source;
        io.dma_length = state.dma_length;
        io.dma_target = state.dma_target;
        io.program_offset = state.program_offset;
        io.program_page = state.page_number;
        io.entry_pc = state.program_counter;
        io.rom_wait = state.rom_speed;
        io.ram_wait = state.ram_speed;
        io.irq_masked = state.irq_disable;
        io.dma_pending = state.dma;
        io.cache_loading = state.cache_loading;
        io.preload_page = state.cache_preload;
        io.suspended = state.suspend;
        io.suspend_remaining = state.suspend_cycles;
        let n = io.vectors.len().min(state.vectors.len());
        io.vectors[..n].copy_from_slice(&state.vectors[..n]);

        for (page, (&locked, &number)) in core
            .cache
            .iter_mut()
            .zip(state.cache_lock.iter().zip(state.cache_page_number.iter()))
        {
            page.locked = locked;
            page.page = number;
        }
        // A short or long cache blob keeps whatever it does not cover.
        for (page, chunk) in core.cache.iter_mut().zip(state.cache_data.chunks(256)) {
            let n = page.words.len().min(chunk.len());
            page.words[..n].copy_from_slice(&chunk[..n]);
        }
        let n = core.data_ram.len().min(state.data_ram.len());
        core.data_ram[..n].copy_from_slice(&state.data_ram[..n]);
        core.cycles = state.cx4_cycles;

        // The converter's ratio comes from this region's master clock; only
        // its moving anchor is saved.
        let master_clock_hz = if master_clock_hz == 0 {
            DEFAULT_MASTER_CLOCK_HZ
        } else {
            master_clock_hz
        };
        self.sync = IntegerRateConverter::with_state(
            CX4_CLOCK_HZ,
            master_clock_hz,
            state.sync.reference_cycles,
            state.sync.samples_at_reference,
            state.sync.remainder,
        );
        self.accesses = state.accesses;
    }

    fn debug_state(&self) -> ExpansionChipState {
        let reg = |address: u32, name: &str, value: u32, description: &str| ChipRegister {
            address,
            name: name.to_string(),
            value,
            description: description.to_string(),
        };
        let io = &self.core.io;
        let registers = vec![
            reg(0x7f40, "DMASRC", io.dma_source, "DMA source address"),
            reg(0x7f45, "DMADST", io.dma_target, "DMA target address"),
            reg(0x7f43, "DMALEN", u32::from(io.dma_length), "DMA length"),
            reg(
                0x7f49,
                "PROGOFF",
                io.program_offset,
                "Program offset in ROM",
            ),
            reg(
                0x7f4d,
                "PAGE",
                u32::from(io.program_page),
                "Program page number",
            ),
            reg(
                0x7f53,
                "STATUS",
                u32::from(self.core.status()),
                "bus/busy/irq/suspend",
            ),
        ];
        ExpansionChipState {
            chip_type: ExpansionChipType::CX4,
            registers,
            internal_ram: Some(self.core.data_ram.to_vec()),
            program_counter: Some(self.core.pc),
            cycle_count: self.core.cycles,
        }
    }

    /// Snapshot the HG51B169 register file for the debugger's slot-2 CPU view
    /// (via the shared [`cpu_state_hg51b`] helper). The `cpu_id` stays 0 here;
    /// the console stamps the real slot index when it places this in the
    /// CPU-state vector.
    fn secondary_cpu_state(&self) -> Option<CpuState> {
        Some(cpu_state_hg51b(&self.core))
    }

    /// Single-step the HG51B169 by one instruction (see [`Self::step_one`]).
    fn step_secondary_cpu(&mut self) -> Option<StepResult> {
        Some(self.step_one())
    }

    /// Forward-decode `count` HG51B169 instructions from program address
    /// `addr`, side-effect-free (each word from the loaded cache page, or from
    /// cart ROM for an uncached page) via the pure [`instructions::disassemble`].
    fn disassemble_secondary(&self, addr: u32, count: u32) -> Vec<Instruction> {
        let mut out = Vec::with_capacity(count as usize);
        let mut pc = addr & 0x7F_FFFF;
        for _ in 0..count {
            let word = program_word(&self.core, &self.rom, pc);
            let d = instructions::disassemble(word);
            out.push(Instruction {
                address: pc,
                bytes: vec![word as u8, (word >> 8) as u8],
                mnemonic: d.mnemonic,
                operands: d.operands,
                comment: None,
            });
            pc = pc.wrapping_add(1) & 0x7F_FFFF;
        }
        out
    }

    /// Arm/disarm HG51B169 instruction-trace capture in the `catch_up` burst.
    /// Disabling drops any captured-but-undrained records so a stale tail can't
    /// leak into a later trace session.
    fn set_secondary_trace_enabled(&mut self, enabled: bool) {
        self.trace_enabled = enabled;
        if !enabled {
            self.trace_buf.clear();
        }
    }

    /// Drain the HG51B169 trace bytes captured since the last call (each a
    /// [`TRACE_ENTRY_SIZE_CX4`](crate::debugger::TRACE_ENTRY_SIZE_CX4)-byte
    /// [`TraceEntryBinaryCx4`]); the console re-parses them into its cpu-2 trace
    /// ring.
    fn take_secondary_trace(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.trace_buf)
    }

    fn label(&self) -> &str {
        "Cx4"
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }

    fn battery(&self) -> Option<&[u8]> {
        // Real MMX2/X3 declare no SRAM (password saves), so `ram` is empty and
        // this returns `None`. A manifest that sets `sram_size > 0` gets a
        // battery-backed `.srm`, mirroring the SPC7110/SA-1 size-driven model.
        if self.has_battery && !self.ram.is_empty() {
            Some(&self.ram)
        } else {
            None
        }
    }

    fn restore_battery(&mut self, data: &[u8]) {
        let n = data.len().min(self.ram.len());
        self.ram[..n].copy_from_slice(&data[..n]);
        self.ram_dirty = false;
    }

    fn battery_dirty(&self) -> bool {
        self.ram_dirty
    }

    fn clear_battery_dirty(&mut self) {
        self.ram_dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_HZ: u64 = 21_477_270;

    fn rom() -> Arc<[u8]> {
        let mut v = vec![0u8; 0x20_0000];
        v[0] = 0xAB;
        v[0x1F_FFFF] = 0xCD;
        Arc::from(v.into_boxed_slice())
    }

    fn chip(ram_size: usize, has_battery: bool) -> Cx4Chip {
        Cx4Chip::new(rom(), ram_size, has_battery, NTSC_HZ)
    }

    /// The chip-address of an S-CPU access to the `$6000-7FFF` register `addr`.
    fn mmio(addr: u16) -> u32 {
        CX4_MMIO_BASE + u32::from(addr - 0x6000)
    }

    /// Build a chip whose ROM holds `program` (little-endian instruction words)
    /// at ROM offset 0, i.e. LoROM `$00:8000`, so a `program_offset` of
    /// `0x008000` points at it.
    fn chip_with_program(program: &[u16]) -> Cx4Chip {
        let mut v = vec![0u8; 0x20_0000];
        for (i, &word) in program.iter().enumerate() {
            v[i * 2] = word as u8;
            v[i * 2 + 1] = (word >> 8) as u8;
        }
        Cx4Chip::new(Arc::from(v.into_boxed_slice()), 0, false, NTSC_HZ)
    }

    #[test]
    fn label_and_type() {
        let c = chip(0, false);
        assert_eq!(c.label(), "Cx4");
        assert_eq!(c.info().chip_type, ExpansionChipType::CX4);
    }

    #[test]
    fn rom_window_reads_real_bytes() {
        let mut c = chip(0, false);
        assert_eq!(c.read(CX4_ROM_BASE, 0), 0xAB);
        assert_eq!(c.read(CX4_ROM_BASE + 0x1F_FFFF, 0), 0xCD);
    }

    #[test]
    fn data_ram_round_trips_through_6000_and_7000() {
        let mut c = chip(0, false);
        // $6000 and $7000 reach the same data RAM byte.
        c.write(CX4_MMIO_BASE, 0x5A, 0);
        assert_eq!(c.read(CX4_MMIO_BASE, 0), 0x5A);
        // $7000 is 0x1000 into the window, and the data RAM index keeps the
        // low twelve bits: byte 0 again.
        let seven = CX4_MMIO_BASE + (0x7000 - 0x6000);
        assert_eq!(c.read(seven, 0), 0x5A);
        // A byte further into the data RAM.
        let deep = CX4_MMIO_BASE + (0x6BFF - 0x6000);
        c.write(deep, 0x42, 0);
        assert_eq!(c.read(deep, 0), 0x42);
    }

    #[test]
    fn status_reads_idle_so_a_busy_poll_never_hangs() {
        let mut c = chip(0, false);
        let status = CX4_MMIO_BASE + (0x7F53 - 0x6000);
        assert_eq!(c.read(status, 0), 0, "a freshly booted chip reads idle");
    }

    #[test]
    fn registers_round_trip() {
        let mut c = chip(0, false);
        let at = |a: u16| CX4_MMIO_BASE + u32::from(a - 0x6000);
        // 24-bit DMA source across its three bytes.
        c.write(at(0x7F40), 0x11, 0);
        c.write(at(0x7F41), 0x22, 0);
        c.write(at(0x7F42), 0x33, 0);
        assert_eq!(c.read(at(0x7F40), 0), 0x11);
        assert_eq!(c.read(at(0x7F41), 0), 0x22);
        assert_eq!(c.read(at(0x7F42), 0), 0x33);
        // A general register, three bytes.
        c.write(at(0x7F80), 0xDE, 0);
        c.write(at(0x7F81), 0xAD, 0);
        c.write(at(0x7F82), 0xBE, 0);
        assert_eq!(c.read(at(0x7F80), 0), 0xDE);
        assert_eq!(c.read(at(0x7F81), 0), 0xAD);
        assert_eq!(c.read(at(0x7F82), 0), 0xBE);
        // $7FC0-$7FEF reach the same registers.
        assert_eq!(c.read(at(0x7FC0), 0), 0xDE);
    }

    #[test]
    fn no_battery_without_ram() {
        let c = chip(0, false);
        assert!(c.battery().is_none());
    }

    #[test]
    fn manifest_ram_is_battery_backed_when_declared() {
        let mut c = chip(0x2000, true);
        assert_eq!(c.battery().map(<[u8]>::len), Some(0x2000));
        // A write through the cart RAM window surfaces in the battery blob.
        c.write(CX4_RAM_BASE + 0x100, 0x7E, 0);
        assert_eq!(c.read(CX4_RAM_BASE + 0x100, 0), 0x7E);
        assert!(c.battery_dirty());
        assert_eq!(c.battery().unwrap()[0x100], 0x7E);
    }

    #[test]
    fn ram_declared_without_battery_stays_out_of_srm() {
        let mut c = chip(0x2000, false);
        c.write(CX4_RAM_BASE + 0x10, 0x99, 0);
        // Still works as RAM...
        assert_eq!(c.read(CX4_RAM_BASE + 0x10, 0), 0x99);
        // ...but never persists.
        assert!(c.battery().is_none());
    }

    #[test]
    fn restore_battery_round_trips() {
        let mut c = chip(0x2000, true);
        let mut srm = vec![0u8; 0x2000];
        srm[0] = 0x1A;
        srm[0x1FFF] = 0x2B;
        c.restore_battery(&srm);
        assert_eq!(c.read(CX4_RAM_BASE, 0), 0x1A);
        assert_eq!(c.read(CX4_RAM_BASE + 0x1FFF, 0), 0x2B);
        assert!(!c.battery_dirty(), "restore clears the dirty flag");
    }

    #[test]
    fn accesses_count_reads_and_writes() {
        let mut c = chip(0, false);
        c.read(CX4_MMIO_BASE, 0);
        c.write(CX4_MMIO_BASE, 0, 0);
        assert_eq!(c.accesses(), 2);
    }

    #[test]
    fn savestate_round_trips_core_and_data_ram() {
        // Run a program, save, and check the core and the data RAM come back
        // in a fresh chip.
        let program: &[u16] = &[
            0x6412, // ld a,#0x12
            0xE00C, // st ramdata,a
            0xEC00, // wrraml [#0x00]
            0xFC00, // halt
        ];
        let mut c = chip_with_program(program);
        c.write(mmio(0x7F4A), 0x80, 0); // program_offset = 0x008000
        c.write(mmio(0x7F4F), 0x00, 0); // start
        c.catch_up(1_000_000); // run to halt
        c.core.registers[7] = 0x12_3456; // a distinctive register value
        let cycles_before = c.core.cycles;
        let accesses_before = c.accesses();

        let blob = c.serialize();
        assert!(!blob.is_empty());

        let mut restored = chip(0, false);
        restored.restore(&blob, NTSC_HZ);
        assert_eq!(restored.core.data_ram[0], 0x12, "data RAM survived");
        assert_eq!(restored.core.registers[7], 0x12_3456, "R7 survived");
        assert_eq!(restored.core.pc, c.core.pc, "PC survived");
        assert!(restored.core.halted);
        assert_eq!(
            restored.core.cycles, cycles_before,
            "cycle counter survived"
        );
        assert_eq!(
            restored.accesses(),
            accesses_before,
            "access count survived"
        );

        // Re-serializing the restored chip yields byte-identical output.
        assert_eq!(restored.serialize(), blob, "round-trip is byte-stable");
    }

    #[test]
    fn restore_ignores_garbage_and_foreign_variants() {
        let mut c = chip(0, false);
        c.core.accumulator = 0x00_00AB;
        // Garbage bytes: dropped, chip untouched.
        c.restore(&[0xFF, 0x00, 0x99], NTSC_HZ);
        assert_eq!(
            c.core.accumulator, 0x00_00AB,
            "garbage blob left the chip intact"
        );

        // A foreign coprocessor variant is also dropped.
        let foreign =
            bincode::serialize(&CoprocessorState::Sa1(crate::savestate::Sa1State::default()))
                .unwrap();
        c.restore(&foreign, NTSC_HZ);
        assert_eq!(c.core.accumulator, 0x00_00AB, "foreign variant dropped");
    }

    #[test]
    fn debug_state_exposes_data_ram_and_type() {
        let c = chip(0, false);
        let s = c.debug_state();
        assert_eq!(s.chip_type, ExpansionChipType::CX4);
        assert_eq!(
            s.internal_ram.as_ref().map(Vec::len),
            Some(hg51b::DATA_RAM_SIZE)
        );
    }

    // ---- the debugger's view of the chip as a second CPU ----------------------

    #[test]
    fn secondary_cpu_state_snapshots_the_register_file() {
        let mut c = chip(0, false);
        c.core.accumulator = 0x12_3456;
        c.core.product_high = 0x0A_BCDE;
        c.core.product_low = 0x00_1234;
        c.core.registers[4] = 0x00_BEEF; // register #7 in the roster (R4)
        c.core.page = 0x0055;
        c.core.pc = 0x00_1234;
        c.core.flags.negative = true;
        c.core.flags.carry = true;

        let st = c
            .secondary_cpu_state()
            .expect("a Cx4 cart exposes the HG51B169 as a secondary CPU");
        // Name contract with debugger-core `COLUMN_SPECS.hg51b`: A/ACCH/ACCL,
        // then R0..R15, then P (20 rows). A drift here breaks the register panel
        // silently, so pin the exact roster.
        let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names.len(), 20, "A/ACCH/ACCL + 16 GP regs + P");
        assert_eq!(&names[0..3], &["A", "ACCH", "ACCL"]);
        assert_eq!(names[3], "R0");
        assert_eq!(names[7], "R4", "register #7 is R4");
        assert_eq!(names[18], "R15");
        assert_eq!(names[19], "P");
        assert_eq!(st.registers[0].value, 0x12_3456, "A snapshot");
        assert_eq!(st.registers[7].value, 0x00_BEEF, "R4 snapshot");
        assert_eq!(st.pc, 0x00_1234, "the program counter");
        // Flags pack N=0x8/Z=0x4/V=0x2/C=0x1 -> N + C = 0x9.
        assert_eq!(st.flags, 0x9, "flags pack N and C");
    }

    #[test]
    fn step_secondary_cpu_runs_one_instruction_at_a_time() {
        let program: &[u16] = &[
            0x6412, // ld a,#0x12
            0xE00C, // st ramdata,a
            0xEC00, // wrraml [#0x00]
            0xFC00, // halt
        ];
        let mut c = chip_with_program(program);
        c.write(mmio(0x7F4A), 0x80, 0); // program_offset = 0x008000
        c.write(mmio(0x7F4F), 0x00, 0); // start
        assert!(!c.core.halted, "the $7F4F write started the core");

        for _ in 0..4 {
            let step = c
                .step_secondary_cpu()
                .expect("the HG51B169 is a steppable secondary CPU");
            assert!(step.instruction_executed, "a running core executes a step");
            assert!(step.cycles > 0, "an executed step spends clocks");
        }
        assert!(c.core.halted, "ran to the halt instruction");
        assert_eq!(c.core.data_ram[0], 0x12, "the program wrote data RAM");

        // A halted core steps nothing.
        let step = c.step_secondary_cpu().unwrap();
        assert!(!step.instruction_executed, "a halted core steps nothing");
        assert_eq!(step.cycles, 0, "a halted step spends no clocks");
    }

    #[test]
    fn disassemble_secondary_decodes_from_rom_and_cache() {
        let program: &[u16] = &[
            0x6412, // ld a,#0x12
            0xE00C, // st ramdata,a
            0xEC00, // wrraml [#0x00]
            0xFC00, // halt
        ];
        let mut c = chip_with_program(program);
        c.write(mmio(0x7F4A), 0x80, 0); // program_offset = 0x008000

        // Uncached: the words are re-derived from cart ROM.
        let rows = c.disassemble_secondary(0, 4);
        assert_eq!(rows.len(), 4, "decoded four instructions");
        assert_eq!(rows[0].address, 0);
        assert_eq!(rows[0].mnemonic, "ld");
        assert_eq!(rows[1].address, 1, "advanced one program word");
        assert_eq!(rows[3].mnemonic, "halt");

        // Cached: after a run the words come from the loaded cache page.
        c.write(mmio(0x7F4F), 0x00, 0); // start
        c.catch_up(1_000_000); // run to halt, filling cache page 0
        let cached = c.disassemble_secondary(0, 4);
        assert_eq!(cached[0].mnemonic, "ld");
        assert_eq!(cached[3].mnemonic, "halt");
    }

    #[test]
    fn trace_capture_records_and_formats_entries() {
        use crate::debugger::{TraceEntryBinaryCx4, TRACE_ENTRY_SIZE_CX4};
        use crate::trace_format::format_trace_line_cx4;

        let program: &[u16] = &[
            0x6412, // ld a,#0x12
            0xE00C, // st ramdata,a
            0xEC00, // wrraml [#0x00]
            0xFC00, // halt
        ];
        let mut c = chip_with_program(program);
        c.write(mmio(0x7F4A), 0x80, 0); // program_offset = 0x008000
        c.set_secondary_trace_enabled(true);
        c.write(mmio(0x7F4F), 0x00, 0); // start
        c.catch_up(1_000_000); // run to halt

        let raw = c.take_secondary_trace();
        assert!(!raw.is_empty(), "tracing captured HG51B169 instructions");
        assert_eq!(raw.len() % TRACE_ENTRY_SIZE_CX4, 0, "whole records only");

        // Every record parses + formats, and the first opcode is the real `ld`
        // (proving the capture is the running program, not an idle NOP), while
        // the last is the `halt`.
        let mut lines = Vec::new();
        for chunk in raw.as_chunks::<TRACE_ENTRY_SIZE_CX4>().0 {
            let e = TraceEntryBinaryCx4::from_bytes(chunk).expect("record parses");
            lines.push(format_trace_line_cx4(&e));
        }
        assert!(
            lines[0].contains("ld"),
            "first traced op is ld: {:?}",
            lines[0]
        );
        assert!(
            lines.last().unwrap().contains("halt"),
            "last traced op is halt: {:?}",
            lines.last()
        );
        // The `ld a,#0x12` sets the accumulator; the very next entry's A column
        // must show it, proving the register snapshot tracks execution.
        assert!(lines[1].contains("A:000012"), "A after ld: {:?}", lines[1]);

        // Disabling tracing drops the buffer.
        c.set_secondary_trace_enabled(false);
        assert!(c.take_secondary_trace().is_empty());
    }

    // ---- catch-up timing, the interrupt, bus contention -------------------------

    #[test]
    fn catch_up_absorbs_idle_at_the_20mhz_ratio() {
        let mut c = chip(0, false);
        // One second of master cycles converts to exactly 20e6 chip cycles; an
        // idle (halted) chip absorbs that whole budget without stepping.
        c.catch_up(NTSC_HZ);
        assert_eq!(c.core.cycles, CX4_CLOCK_HZ, "idle chip absorbs the budget");
        // Monotonic across a second catch-up.
        c.catch_up(2 * NTSC_HZ);
        assert_eq!(c.core.cycles, 2 * CX4_CLOCK_HZ);
        // A target at/behind the anchor is a no-op.
        c.catch_up(NTSC_HZ);
        assert_eq!(c.core.cycles, 2 * CX4_CLOCK_HZ);
    }

    #[test]
    fn live_7f4f_runs_a_program_through_catch_up() {
        // A program the S-CPU starts with its `$7F4F` write, run by `catch_up`.
        let program: &[u16] = &[
            0x6412, // ld a,#0x12
            0xE00C, // st ramdata,a
            0xEC00, // wrraml [#0x00]
            0x6434, // ld a,#0x34
            0xE00C, // st ramdata,a
            0xEC01, // wrraml [#0x01]
            0xFC00, // halt
        ];
        let mut c = chip_with_program(program);
        c.write(mmio(0x7F49), 0x00, 0); // program_offset low
        c.write(mmio(0x7F4A), 0x80, 0); // program_offset mid -> 0x008000
        c.write(mmio(0x7F4B), 0x00, 0); // program_offset high
        c.write(mmio(0x7F4D), 0x00, 0); // program page low
        c.write(mmio(0x7F4E), 0x00, 0); // program page high
        c.write(mmio(0x7F4F), 0x00, 0); // entry point 0 -> start
        assert!(!c.core.halted, "the $7F4F write started the core");

        c.catch_up(1_000_000); // ~1 ms of master cycles: runs to halt
        assert!(c.core.halted, "the program ran to its halt");
        assert_eq!(c.core.data_ram[0], 0x12);
        assert_eq!(c.core.data_ram[1], 0x34);
    }

    #[test]
    fn catch_up_self_waits_after_an_overshoot() {
        // One step (the 256-word cache-page load) costs far more than
        // a handful-of-cycles budget, so the core overshoots; a later small
        // catch-up must not step it further until the master clock reaches it.
        let mut c = chip_with_program(&[0xFC00]); // halt at pc 0
        c.write(mmio(0x7F4A), 0x80, 0); // program_offset = 0x008000
        c.write(mmio(0x7F4F), 0x00, 0); // start

        c.catch_up(10); // target ~ 9 chip cycles
        let after_first = c.core.cycles;
        assert!(after_first > 100, "the page load overshot the tiny budget");

        c.catch_up(20); // target still far below the overshoot
        assert_eq!(c.core.cycles, after_first, "self-wait: no further stepping");
    }

    #[test]
    fn irq_rises_on_dma_completion_and_clears() {
        let mut c = chip_with_program(&[]); // all-zero ROM is fine for a DMA
        c.write(mmio(0x7F40), 0x00, 0); // dma source low
        c.write(mmio(0x7F41), 0x80, 0); // dma source mid -> 0x008000
        c.write(mmio(0x7F42), 0x00, 0); // dma source high
        c.write(mmio(0x7F43), 0x04, 0); // dma length = 4
        c.write(mmio(0x7F44), 0x00, 0);
        c.write(mmio(0x7F45), 0x00, 0); // dma target = data RAM 0
        c.write(mmio(0x7F46), 0x00, 0);
        c.write(mmio(0x7F47), 0x00, 0); // dma target high -> asks for the DMA
        assert!(c.core.io.dma_pending, "the $7F47 write asked for the DMA");

        c.catch_up(1_000_000);
        assert!(!c.core.busy(), "idle after the DMA");
        assert!(c.has_irq(), "going idle raised the interrupt");

        c.write(mmio(0x7F5E), 0x00, 0); // acknowledge the interrupt
        assert!(!c.has_irq(), "$7F5E cleared the interrupt");
    }

    #[test]
    fn has_irq_respects_the_mask() {
        let mut c = chip(0, false);
        c.core.irq_pending = true;
        assert!(c.has_irq());
        // $7F51 bit 0 masks the line and drops the pending interrupt.
        c.write(mmio(0x7F51), 0x01, 0);
        assert!(!c.has_irq(), "the mask hides and drops the interrupt");
        assert!(!c.core.irq_pending);
    }

    #[test]
    fn rom_read_contends_while_the_chip_owns_the_bus() {
        let mut v = vec![0u8; 0x20_0000];
        v[0x100] = 0x11; // a normal ROM byte
        v[0x7FC0] = 0x22; // inside the reset/IRQ vector window ($00:FFC0)
        let mut c = Cx4Chip::new(Arc::from(v.into_boxed_slice()), 0, false, NTSC_HZ);

        // Idle: the read is transparent.
        assert_eq!(c.rom_read(0x100), 0x11);

        // The chip owns the cart bus (mid-DMA): non-vector reads contend to 0...
        c.core.io.dma_pending = true;
        assert_eq!(
            c.rom_read(0x100),
            0,
            "contended while the chip owns the bus"
        );
        // ...but the vector window stays readable so the CPU can take interrupts.
        assert_eq!(
            c.rom_read(0x7FC0),
            0x22,
            "vector window survives contention"
        );

        // A suspended chip yields the bus back to the CPU.
        c.core.io.suspended = true;
        assert_eq!(c.rom_read(0x100), 0x11, "suspend yields the bus");
    }
}
