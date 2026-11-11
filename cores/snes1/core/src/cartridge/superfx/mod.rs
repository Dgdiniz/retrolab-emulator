//! The Super FX cartridge: the GSU, and the Game Pak ROM and RAM it shares
//! with the S-CPU.
//!
//! The Super FX (Star Fox, Doom, Yoshi's Island, Stunt Race FX) puts a
//! 16-bit RISC processor, the **GSU** ([`Gsu`], in the `superfx-gsu`
//! crate), and its plot engine in the cartridge, beside the console's
//! S-CPU. The two share Game Pak ROM and Game Pak RAM: while the GSU runs
//! with a bus (SCMR's RON and RAN bits), the S-CPU reads a fixed pattern
//! from ROM and open bus from RAM.
//!
//! The chip owns the [`Gsu`] and drives it. Every S-CPU access, and the
//! hook the console runs between instructions, first runs
//! [`SuperFxChip::catch_up`], which steps the GSU against Game Pak ROM and
//! RAM through a [`GsuBus`] until it reaches the S-CPU's master cycle. The
//! GSU runs on the master clock, so each GSU clock moves the anchor one
//! master cycle. Writing `$301F` starts the GSU; STOP halts it and, unless
//! CFGR masks it, raises the S-CPU's IRQ ([`SuperFxChip::has_irq`]), which
//! reading `$3031` clears.
//!
//! ## One chip, three windows, one `chip_id`
//!
//! A single [`SuperFxChip`] answers on three disjoint bus windows, all routed
//! here as [`MapTarget::ChipMmio`](retro_core::cartridge::MapTarget) from
//! [`super::presets::superfx_preset`] and split apart by the `base` sentinel
//! the slot carries:
//!
//! - **Registers** (`base = SUPERFX_MMIO_BASE`): `$3000-$34FF` (`$3000-301F`
//!   R0-R15, `$3030-303F` control and status, `$3100-32FF` the instruction
//!   cache), served by the GSU's own ports ([`Gsu::read_port`],
//!   [`Gsu::write_port`]). The slot folds the source bank away (stride
//!   `0`), so a register access arrives as `SUPERFX_MMIO_BASE + (reg -
//!   0x3000)`; adding `0x3000` gives the register back.
//! - **ROM** (`base = SUPERFX_ROM_BASE`): the LoROM and linear windows. The
//!   S-CPU reads the real ROM while the GSU does not hold it, and a fixed
//!   pattern while it does (`SFR.G` and `SCMR.RON`), which is why the read
//!   passes through the chip.
//! - **Game Pak RAM** (`base = SUPERFX_RAM_BASE`): the `$6000-7FFF` 8 KiB
//!   window plus the `$60-7D`/`$E0-FF` linear views. The chip owns the RAM
//!   (mirrored modulo its size), so the plot engine and the battery work on
//!   one buffer, as the SA-1's BW-RAM does; the S-CPU reads open bus (the
//!   data bus's last value) while the GSU holds it (`SCMR.RAN`).
//!
//! What the S-CPU reads from ROM and RAM while the GSU holds them was
//! checked against bsnes-plus and ares (`CPUROM::read`, `CPURAM::read`).

mod bus;

use std::sync::Arc;

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};
use retro_core::cpu::Addressable;
use retro_core::emulator::StepResult;
use retro_core::types::{CpuState, Instruction};
use superfx_gsu::Gsu;

use self::bus::{GsuBus, GsuPeekBus};
use crate::debugger::{cpu_state_gsu, TraceEntryBinaryGsu};
use crate::savestate::{CoprocessorState, SuperFxPixelCacheState, SuperFxState};

pub use crate::cartridge::chip_ids::{SUPERFX_MMIO_BASE, SUPERFX_RAM_BASE, SUPERFX_ROM_BASE};

/// What the S-CPU reads from Game Pak ROM while the GSU holds the ROM bus
/// (`SFR.G` and `SCMR.RON`), by the low nibble of the address. The same
/// sixteen values as bsnes-plus and ares.
const RON_OPENBUS: [u8; 16] = [
    0x00, 0x01, 0x00, 0x01, 0x04, 0x01, 0x00, 0x01, 0x00, 0x01, 0x08, 0x01, 0x00, 0x01, 0x0c, 0x01,
];

/// The GSU register an S-CPU access to the register window reaches.
fn register_address(offset: u32) -> u16 {
    0x3000 + ((offset.wrapping_sub(SUPERFX_MMIO_BASE) & 0x7FF) as u16)
}

/// The Super FX coprocessor: the GSU, its Game Pak ROM/RAM, and the catch-up
/// anchor that drives the GSU against the S-CPU's master clock.
pub struct SuperFxChip {
    /// The GSU: register file + executor. Driven by [`SuperFxChip::catch_up`].
    gsu: Gsu,
    /// Cartridge ROM, the GSU bus's program/cache/ROM-buffer source and the
    /// S-CPU's ROM window (through [`SUPERFX_ROM_BASE`], for RON arbitration).
    rom: Arc<[u8]>,
    /// Game Pak RAM, owned by the chip (mirrored modulo its length) and shared
    /// by the GSU (via [`GsuBus`]) and the S-CPU RAM window.
    ram: Vec<u8>,
    /// Whether Game Pak RAM is battery-backed (`cartridge_type $15`).
    has_battery: bool,
    /// Whether battery RAM changed since the last `.srm` flush.
    ram_dirty: bool,
    /// GSU catch-up anchor, in the S-CPU master-clock domain. Advanced 1:1 by
    /// the GSU step clocks each burst (the GSU shares the master clock).
    gsu_master: u64,
    /// The last byte the S-CPU put on or took off the chip's data bus: what
    /// it reads from Game Pak RAM while the GSU holds it (`SCMR.RAN`), the
    /// chip's stand-in for the S-CPU's MDR.
    last_mdr: u8,
    /// S-CPU accesses (reads + writes) to the chip since reset.
    accesses: u64,
    /// Whether the debugger asked the chip to capture a GSU instruction trace.
    /// Mirrored from the debugger's cpu-2 tracing flag each frame; the GSU runs
    /// deep inside the `catch_up` burst where the debugger isn't reachable, so
    /// this flag is the bridge (like the APU's `spc_trace_enabled`).
    gsu_trace_enabled: bool,
    /// Serialized [`TraceEntryBinaryGsu`] records captured in `catch_up` since
    /// the last drain. Transient debug state, never saved.
    gsu_trace_buf: Vec<u8>,
    /// Debugger metadata.
    info: ExpansionChipInfo,
}

impl SuperFxChip {
    /// Build a Super FX chip over `rom`, with `ram_size` bytes of Game Pak
    /// RAM (`0` for none). `has_battery` gates whether that RAM persists to
    /// `.srm`. The GSU shares the console's master clock, so `master_clock_hz`
    /// is accepted for signature parity but needs no rate conversion.
    pub fn new(rom: Arc<[u8]>, ram_size: usize, has_battery: bool, _master_clock_hz: u64) -> Self {
        let mut features = vec!["gsu".to_string()];
        if ram_size > 0 {
            features.push("gpram".to_string());
        }
        Self {
            gsu: Gsu::new(),
            rom,
            ram: vec![0u8; ram_size],
            has_battery,
            ram_dirty: false,
            gsu_master: 0,
            last_mdr: 0,
            accesses: 0,
            gsu_trace_enabled: false,
            gsu_trace_buf: Vec::new(),
            info: ExpansionChipInfo {
                name: "Super FX".to_string(),
                chip_type: ExpansionChipType::SuperFX,
                description: "Super FX GSU (RISC CPU + plot engine)".to_string(),
                features,
            },
        }
    }

    /// The cartridge ROM the chip retains for the GSU bus and the S-CPU's
    /// ROM windows. Read-only.
    pub fn rom(&self) -> &[u8] {
        &self.rom
    }

    /// Read a Game Pak RAM byte, mirroring modulo the RAM length.
    fn ram_read(&self, ram_off: u32) -> u8 {
        if self.ram.is_empty() {
            return 0;
        }
        self.ram[(ram_off as usize) % self.ram.len()]
    }

    /// Write a Game Pak RAM byte, mirroring modulo the RAM length.
    fn ram_write(&mut self, ram_off: u32, value: u8) {
        if self.ram.is_empty() {
            return;
        }
        let len = self.ram.len();
        self.ram[(ram_off as usize) % len] = value;
        self.ram_dirty = true;
    }

    /// Read a Game Pak ROM byte for the S-CPU: the fixed [`RON_OPENBUS`]
    /// pattern while the GSU runs with the ROM bus, otherwise the real byte,
    /// folded modulo the ROM length.
    fn scpu_rom_read(&self, rom_off: u32) -> u8 {
        if self.gsu.is_running() && self.gsu.owns_rom_bus() {
            return RON_OPENBUS[(rom_off & 15) as usize];
        }
        if self.rom.is_empty() {
            return 0;
        }
        self.rom[(rom_off as usize) % self.rom.len()]
    }

    /// Read a Game Pak RAM byte for the S-CPU: open bus (the last data-bus
    /// value, [`Self::last_mdr`]) while the GSU runs with the RAM bus,
    /// otherwise the real byte.
    fn scpu_ram_read(&self, ram_off: u32) -> u8 {
        if self.gsu.is_running() && self.gsu.owns_ram_bus() {
            return self.last_mdr;
        }
        self.ram_read(ram_off)
    }

    /// Single-step the GSU by one instruction against the real Game Pak
    /// ROM/RAM bus, advancing the catch-up anchor by the clocks it spent
    /// (the debugger step model, the S-CPU isn't running, so the GSU drives
    /// its own anchor). A halted GSU with no transfer in flight has nothing
    /// to run and reports `instruction_executed: false`, like
    /// [`Sa1Chip::step_one`](crate::cartridge::Sa1Chip).
    fn step_one(&mut self) -> StepResult {
        if !self.gsu.is_running() && !self.gsu.has_transfer_in_flight() {
            return StepResult {
                cycles: 0,
                breakpoint_hit: false,
                instruction_executed: false,
            };
        }
        let mut gsu = std::mem::take(&mut self.gsu);
        let spent = {
            let mut gbus = GsuBus {
                rom: &self.rom,
                ram: &mut self.ram,
                ram_dirty: &mut self.ram_dirty,
            };
            gsu.step(&mut gbus)
        };
        self.gsu = gsu;
        self.gsu_master += u64::from(spent);
        StepResult {
            cycles: spent,
            breakpoint_hit: false,
            instruction_executed: true,
        }
    }
}

impl Coprocessor for SuperFxChip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, master_cycle: u64) -> u8 {
        // Run the GSU up to the S-CPU's cycle first, so a register/RAM/ROM read
        // observes everything the GSU did before this access.
        self.catch_up(master_cycle);
        self.accesses += 1;
        let value = if offset >= SUPERFX_ROM_BASE {
            self.scpu_rom_read(offset - SUPERFX_ROM_BASE)
        } else if offset >= SUPERFX_RAM_BASE {
            self.scpu_ram_read(offset - SUPERFX_RAM_BASE)
        } else {
            self.gsu.read_port(register_address(offset))
        };
        self.last_mdr = value;
        value
    }

    fn write(&mut self, offset: u32, value: u8, master_cycle: u64) {
        self.catch_up(master_cycle);
        self.accesses += 1;
        self.last_mdr = value;
        if offset >= SUPERFX_ROM_BASE {
            // Game Pak ROM is read-only; the S-CPU cannot write it.
        } else if offset >= SUPERFX_RAM_BASE {
            // The S-CPU's writes to Game Pak RAM always land, even while the
            // GSU holds the RAM bus.
            self.ram_write(offset - SUPERFX_RAM_BASE, value);
        } else {
            self.gsu.write_port(register_address(offset), value);
        }
    }

    /// Run the GSU forward to the S-CPU's `master_cycle`. The GSU shares the
    /// master clock, so a GSU step advances the anchor 1:1 (unlike the SA-1,
    /// which runs at master/2). A stopped GSU with no transfer in flight
    /// consumes no observable time, so the anchor jumps straight to the
    /// target rather than looping an idle frame at six clocks a step.
    fn catch_up(&mut self, master_cycle: u64) {
        if master_cycle <= self.gsu_master {
            return;
        }
        if !self.gsu.is_running() && !self.gsu.has_transfer_in_flight() {
            self.gsu_master = master_cycle;
            return;
        }
        // Disjoint borrows: the GSU steps against ROM/RAM while the anchor and
        // dirty flag are tracked separately (no whole-chip borrow, so no
        // `mem::take` dance is needed beyond lifting the GSU out).
        let mut gsu = std::mem::take(&mut self.gsu);
        let mut anchor = self.gsu_master;
        {
            let mut gbus = GsuBus {
                rom: &self.rom,
                ram: &mut self.ram,
                ram_dirty: &mut self.ram_dirty,
            };
            // Execute one instruction at a time until the GSU reaches the S-CPU
            // or halts (`stop` / `SFR.G` cleared).
            while anchor < master_cycle && gsu.is_running() {
                if self.gsu_trace_enabled {
                    Self::capture_gsu_trace(&mut self.gsu_trace_buf, &gsu, &mut gbus, anchor);
                }
                anchor += u64::from(gsu.step(&mut gbus));
            }
            // A GSU that halted mid-burst still has its last ROM read or RAM
            // write in flight; a stopped GSU spends six clocks a step, which
            // lands it.
            while !gsu.is_running() && gsu.has_transfer_in_flight() {
                anchor += u64::from(gsu.step(&mut gbus));
            }
        }
        // A halted GSU will not advance again until the S-CPU restarts it:
        // absorb the remaining gap so the next catch-up is a cheap no-op.
        if !gsu.is_running() && anchor < master_cycle {
            anchor = master_cycle;
        }
        self.gsu_master = anchor;
        self.gsu = gsu;
    }

    fn reset(&mut self, _master_clock_hz: u64) {
        // The GSU returns to its power-on state (VCR = $04, pipeline primed
        // with NOP, cache invalidated, registers cleared). Game Pak RAM
        // (battery-backed or static scratch) survives a soft reset like the
        // SA-1's BW-RAM.
        self.gsu = Gsu::new();
        self.gsu_master = 0;
        self.last_mdr = 0;
        self.accesses = 0;
        // Drop any captured-but-undrained trace records; the enable flag is
        // re-synced from the debugger each frame.
        self.gsu_trace_buf.clear();
    }

    /// Volatile savestate: the GSU register file + ROM/RAM buffer latches +
    /// instruction cache + plot pixel caches + the access counter, and Game
    /// Pak RAM. The ROM is immutable, so it is not captured. The savestate
    /// keeps its own field names (`sfr`, `pbr`, `romcl`, ...), not the GSU's.
    fn serialize(&self) -> Vec<u8> {
        let g = &self.gsu;
        let state = CoprocessorState::SuperFx(SuperFxState {
            r: g.registers,
            sfr: g.status,
            pbr: g.program_bank,
            rombr: g.rom_bank,
            rambr: g.ram_bank,
            cbr: g.cache_base,
            scbr: g.screen_base,
            scmr: g.screen_mode,
            colr: g.color,
            por: g.plot_options,
            bramr: g.backup_ram_enabled,
            vcr: g.version,
            cfgr: g.config,
            clsr_divider: g.clock_divider,
            pipeline: g.next_opcode,
            ramaddr: g.last_ram_address,
            sreg: g.source,
            dreg: g.destination,
            romcl: g.rom_read_delay,
            romdr: g.rom_data,
            ramar: g.ram_write_address,
            ramcl: g.ram_write_delay,
            ramdr: g.ram_write_data,
            cache_buffer: g.cache.bytes.to_vec(),
            cache_valid: g.cache.valid.to_vec(),
            pixelcache: g
                .pixel_cache
                .iter()
                .map(|line| SuperFxPixelCacheState {
                    offset: line.group,
                    bitpend: line.pending,
                    data: line.colors,
                })
                .collect(),
            cycles_executed: g.cycles,
            gsu_master: self.gsu_master,
            last_mdr: self.last_mdr,
            accesses: self.accesses,
            r15_modified: g.jumped,
            ram: self.ram.clone(),
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        // A garbage/empty blob or a foreign variant decodes to something we
        // can't consume and is dropped, leaving the chip untouched.
        let Ok(CoprocessorState::SuperFx(state)) = bincode::deserialize::<CoprocessorState>(data)
        else {
            return;
        };
        let g = &mut self.gsu;
        g.registers = state.r;
        g.status = state.sfr;
        g.program_bank = state.pbr;
        g.rom_bank = state.rombr;
        g.ram_bank = state.rambr;
        g.cache_base = state.cbr;
        g.screen_base = state.scbr;
        g.screen_mode = state.scmr;
        g.color = state.colr;
        g.plot_options = state.por;
        g.backup_ram_enabled = state.bramr;
        g.version = state.vcr;
        g.config = state.cfgr;
        g.clock_divider = state.clsr_divider;
        g.next_opcode = state.pipeline;
        g.last_ram_address = state.ramaddr;
        g.source = state.sreg;
        g.destination = state.dreg;
        g.rom_read_delay = state.romcl;
        g.rom_data = state.romdr;
        g.ram_write_address = state.ramar;
        g.ram_write_delay = state.ramcl;
        g.ram_write_data = state.ramdr;
        // Length-tolerant copies: a short/long blob keeps the untouched tail.
        let n = g.cache.bytes.len().min(state.cache_buffer.len());
        g.cache.bytes[..n].copy_from_slice(&state.cache_buffer[..n]);
        let n = g.cache.valid.len().min(state.cache_valid.len());
        g.cache.valid[..n].copy_from_slice(&state.cache_valid[..n]);
        for (line, saved) in g.pixel_cache.iter_mut().zip(state.pixelcache.iter()) {
            line.group = saved.offset;
            line.pending = saved.bitpend;
            line.colors = saved.data;
        }
        g.cycles = state.cycles_executed;
        g.jumped = state.r15_modified;
        // Length-tolerant Game Pak RAM copy: a blob from a differently-sized
        // cart keeps this chip's untouched tail rather than resizing.
        let n = self.ram.len().min(state.ram.len());
        self.ram[..n].copy_from_slice(&state.ram[..n]);
        self.gsu_master = state.gsu_master;
        self.last_mdr = state.last_mdr;
        self.accesses = state.accesses;
    }

    fn debug_state(&self) -> ExpansionChipState {
        let reg = |address: u32, name: &str, value: u32, description: &str| ChipRegister {
            address,
            name: name.to_string(),
            value,
            description: description.to_string(),
        };
        let g = &self.gsu;
        let mut registers = Vec::with_capacity(33);
        for n in 0..16 {
            registers.push(reg(
                0x3000 + (n as u32) * 2,
                &format!("R{n}"),
                u32::from(g.registers[n]),
                "General-purpose register (R15 = PC)",
            ));
        }
        #[rustfmt::skip]
        let rows: [(u32, &str, u32, &str); 17] = [
            (0x3030, "SFR", u32::from(g.status), "Status flag register (Z/CY/S/OV/G/R/ALT/IRQ)"),
            (0x3034, "PBR", u32::from(g.program_bank), "Program bank"),
            (0x3036, "ROMBR", u32::from(g.rom_bank), "ROM bank"),
            (0x303C, "RAMBR", u32::from(g.ram_bank), "Game Pak RAM bank"),
            (0x303A, "SCMR", u32::from(g.screen_mode), "Screen mode (RON/RAN + plot depth)"),
            (0x303E, "CBR", u32::from(g.cache_base), "Cache base register"),
            (0x3038, "SCBR", u32::from(g.screen_base), "Screen base"),
            (0x3037, "CFGR", u32::from(g.config), "Config (IRQ mask + multiplier speed)"),
            (0x3039, "CLSR", u32::from(g.clock_divider), "Clock select (divider: 2 = 10.7 MHz, 1 = 21.4 MHz)"),
            (0x3040, "COLR", u32::from(g.color), "Plot colour latch (COLOR)"),
            (0x3041, "POR", u32::from(g.plot_options), "Plot option (dither/transparency/nibble)"),
            (0x3031, "IRQ", u32::from(g.irq_pending()), "STOP interrupt latched to the S-CPU"),
            (0x3042, "ROMCL", u32::from(g.rom_read_delay), "ROM buffer read latency"),
            (0x3043, "ROMDR", u32::from(g.rom_data), "ROM buffer data"),
            (0x3044, "RAMAR", u32::from(g.ram_write_address), "RAM buffer address"),
            (0x3045, "RAMCL", u32::from(g.ram_write_delay), "RAM buffer write latency"),
            (0x3046, "RAMDR", u32::from(g.ram_write_data), "RAM buffer data"),
        ];
        for (address, name, value, description) in rows {
            registers.push(reg(address, name, value, description));
        }
        ExpansionChipState {
            chip_type: ExpansionChipType::SuperFX,
            registers,
            internal_ram: (!self.ram.is_empty()).then(|| self.ram.clone()),
            program_counter: Some(u32::from(g.pc())),
            cycle_count: g.cycles,
        }
    }

    fn label(&self) -> &str {
        "Super FX"
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }

    /// The GSU asserts the S-CPU IRQ line when `stop` executes unmasked
    /// (`SFR.IRQ`); the S-CPU clears it by reading `$3031` (handled by
    /// [`Gsu::read_port`]). The per-instruction burst hook ORs this into the
    /// S-CPU IRQ line, so a `stop` inside a catch-up burst surfaces here.
    fn has_irq(&self) -> bool {
        self.gsu.irq_pending()
    }

    /// Register snapshot of the GSU for the debugger's extra CPU slot, via the
    /// shared [`cpu_state_gsu`] helper. The `cpu_id` stays 0 here; the console
    /// stamps the real slot index when it places this in the CPU-state vector.
    fn secondary_cpu_state(&self) -> Option<CpuState> {
        Some(cpu_state_gsu(&self.gsu))
    }

    /// Single-step the GSU by one instruction (see [`Self::step_one`]).
    fn step_secondary_cpu(&mut self) -> Option<StepResult> {
        Some(self.step_one())
    }

    /// Arm/disarm GSU instruction-trace capture in the `catch_up` burst.
    /// Disabling drops any captured-but-undrained records so a stale tail can't
    /// leak into a later trace session.
    fn set_secondary_trace_enabled(&mut self, enabled: bool) {
        self.gsu_trace_enabled = enabled;
        if !enabled {
            self.gsu_trace_buf.clear();
        }
    }

    /// Drain the GSU trace bytes captured since the last call (each a
    /// [`TRACE_ENTRY_SIZE_GSU`](crate::debugger::TRACE_ENTRY_SIZE_GSU)-byte
    /// [`TraceEntryBinaryGsu`]); the console re-parses them into its cpu-2
    /// trace ring.
    fn take_secondary_trace(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.gsu_trace_buf)
    }

    /// Forward-decode `count` GSU instructions from `addr`, side-effect-free,
    /// against a read-inert [`GsuPeekBus`]. The GSU is prefix-sensitive, so the
    /// decode threads the live `SFR.ALT1`/`SFR.ALT2` state (a debugger
    /// convenience, like the SA-1's static M/X threading, it does not re-derive
    /// the prefix per row).
    fn disassemble_secondary(&self, addr: u32, count: u32) -> Vec<Instruction> {
        let alt1 = self.gsu.alt1();
        let alt2 = self.gsu.alt2();
        let mut out = Vec::with_capacity(count as usize);
        let mut cursor = addr & 0x7F_FFFF;
        let mut peek = GsuPeekBus {
            rom: &self.rom,
            ram: &self.ram,
        };
        for _ in 0..count {
            let res = superfx_gsu::disasm::disassemble_one(&mut peek, cursor, alt1, alt2);
            let len = u32::from(res.bytes_consumed.max(1));
            out.push(Instruction {
                address: cursor,
                bytes: res.bytes,
                mnemonic: res.mnemonic,
                operands: res.operands,
                comment: None,
            });
            cursor = cursor.wrapping_add(len) & 0x7F_FFFF;
        }
        out
    }

    fn battery(&self) -> Option<&[u8]> {
        // Only a `$15` cart (Yoshi's Island) declares battery-backed Game Pak
        // RAM; Star Fox's 32 KiB is volatile scratch RAM that must not persist.
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

/// Bus ownership: whether the GSU claims the Game Pak ROM/RAM buses. These
/// read SCMR alone; the S-CPU's read path also asks whether the GSU is
/// running ([`Self::scpu_rom_read`], [`Self::scpu_ram_read`]), since the
/// S-CPU is only locked out while the GSU executes.
impl SuperFxChip {
    /// `true` iff the GSU currently claims the Game Pak ROM bus (`SCMR.RON`).
    pub fn gsu_owns_rom(&self) -> bool {
        self.gsu.owns_rom_bus()
    }

    /// `true` iff the GSU currently claims the Game Pak RAM bus (`SCMR.RAN`).
    pub fn gsu_owns_ram(&self) -> bool {
        self.gsu.owns_ram_bus()
    }

    /// Append a pre-execution [`TraceEntryBinaryGsu`] for the instruction the
    /// GSU is about to run to `buf`. The opcode is the pipeline byte (the byte
    /// `gsu.step` will execute); its operands are read from the live bus at
    /// `PBR:R15`, exactly where the executor's next operand fetch reads them,
    /// so a delay-slot instruction records the operand bytes the hardware
    /// actually consumes (from the branch target), not the ones textually
    /// after it. The display address is `PBR:(R15-1)`, the opcode's own fetch
    /// address, which also makes relative-branch targets decode correctly.
    /// Reads only (never marks Game Pak RAM dirty).
    fn capture_gsu_trace(buf: &mut Vec<u8>, gsu: &Gsu, bus: &mut GsuBus<'_>, anchor: u64) {
        let bank = u32::from(gsu.program_bank);
        let pc = gsu.registers[15];
        let operand1 = bus.read((bank << 16) | u32::from(pc));
        let operand2 = bus.read((bank << 16) | u32::from(pc.wrapping_add(1)));
        let entry = TraceEntryBinaryGsu {
            master_cycle: anchor,
            pbr_pc: (bank << 16) | u32::from(pc.wrapping_sub(1)),
            sfr: gsu.status,
            rombr: gsu.rom_bank,
            rambr: u8::from(gsu.ram_bank),
            bytes_len: 3,
            bytes: [gsu.next_opcode, operand1, operand2],
            r: gsu.registers,
        };
        buf.extend_from_slice(&entry.to_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use superfx_gsu::registers::screen_mode;

    const NTSC_HZ: u64 = 21_477_270;

    fn chip(ram_size: usize, has_battery: bool) -> SuperFxChip {
        SuperFxChip::new(
            Arc::from(vec![0u8; 0x10_0000].into_boxed_slice()),
            ram_size,
            has_battery,
            NTSC_HZ,
        )
    }

    /// The chip-address the chip's `read`/`write` receives for an MMIO
    /// register `reg` in `$3000-$34FF` (the bus forms `base + (reg - 0x3000)`).
    fn mmio(reg: u16) -> u32 {
        SUPERFX_MMIO_BASE + u32::from(reg - 0x3000)
    }

    /// Build a chip whose Game Pak ROM starts with `kernel` (rest zero-filled
    /// to `rom_len`) plus `ram_size` bytes of Game Pak RAM, so a burst runs
    /// real code against the real bus.
    fn chip_running_kernel(kernel: &[u8], rom_len: usize, ram_size: usize) -> SuperFxChip {
        let mut rom = vec![0u8; rom_len];
        rom[..kernel.len()].copy_from_slice(kernel);
        SuperFxChip::new(Arc::from(rom.into_boxed_slice()), ram_size, false, NTSC_HZ)
    }

    /// Set `R15 = entry` and start the GSU (`SFR.G`) over MMIO, at master
    /// cycle 0 so the write does not itself run a burst.
    fn arm_and_go(c: &mut SuperFxChip, entry: u16) {
        c.write(mmio(0x301E), (entry & 0xFF) as u8, 0);
        c.write(mmio(0x301F), (entry >> 8) as u8, 0);
    }

    /// Read a 16-bit GSU register over MMIO at master cycle 0 (a no-op
    /// catch-up), so the observation never itself advances the GSU.
    fn read_reg16(c: &mut SuperFxChip, reg: u16) -> u16 {
        let lo = u16::from(c.read(mmio(reg), 0));
        let hi = u16::from(c.read(mmio(reg + 1), 0));
        lo | (hi << 8)
    }

    #[test]
    fn new_reports_super_fx_metadata() {
        let c = chip(0x8000, true);
        assert_eq!(c.label(), "Super FX");
        assert_eq!(c.info().chip_type, ExpansionChipType::SuperFX);
        assert!(c.info().features.iter().any(|f| f == "gsu"));
        assert!(c.info().features.iter().any(|f| f == "gpram"));
        assert_eq!(c.rom().len(), 0x10_0000);
    }

    #[test]
    fn general_registers_round_trip_through_mmio() {
        let mut c = chip(0, false);
        // R0-R14 round-trip both bytes cleanly (R15 has the GO side effect).
        for n in 0..15u16 {
            let lo = mmio(0x3000 + n * 2);
            let hi = mmio(0x3001 + n * 2);
            c.write(lo, 0x34, 0);
            c.write(hi, 0x12, 0);
            assert_eq!(c.read(lo, 0), 0x34, "R{n} low byte");
            assert_eq!(c.read(hi, 0), 0x12, "R{n} high byte");
        }
    }

    #[test]
    fn writing_r15_high_byte_starts_the_gsu() {
        let mut c = chip(0, false);
        // SFR.G clear at reset.
        assert_eq!(c.read(mmio(0x3030), 0) & 0x20, 0, "GO clear after reset");
        // Writing R15 low byte alone does not start the GSU.
        c.write(mmio(0x301E), 0x00, 0);
        assert_eq!(c.read(mmio(0x3030), 0) & 0x20, 0, "GO still clear");
        // Writing R15 high byte ($301F) sets SFR.G.
        c.write(mmio(0x301F), 0x80, 0);
        assert_ne!(c.read(mmio(0x3030), 0) & 0x20, 0, "GO set by $301F write");
        assert_eq!(c.read(mmio(0x301F), 0), 0x80, "R15 high byte stored");
    }

    #[test]
    fn control_registers_round_trip() {
        let mut c = chip(0, false);
        c.write(mmio(0x3034), 0x42, 0); // PBR (also flushes cache)
        c.write(mmio(0x303A), 0x18, 0); // SCMR = RON | RAN (md = 0)
                                        // PBR is the one control register the S-CPU both writes and reads back.
        assert_eq!(c.read(mmio(0x3034), 0), 0x42, "PBR round-trips");
        // SCMR is write-only over MMIO but drives the RON/RAN arbitration helpers.
        assert!(c.gsu_owns_rom(), "SCMR.RON set");
        assert!(c.gsu_owns_ram(), "SCMR.RAN set");
        // Read-only status registers report power-on values: VCR = $04, and the
        // ROM/RAM bank registers start at bank 0 (the GSU sets them internally,
        // never the S-CPU over MMIO).
        assert_eq!(c.read(mmio(0x303B), 0), 0x04, "VCR = $04");
        assert_eq!(c.read(mmio(0x3036), 0), 0x00, "ROMBR = 0 at reset");
        assert_eq!(c.read(mmio(0x303C), 0), 0x00, "RAMBR = 0 at reset");
        // BRAMR ($3033) and RAMBR ($303C) are distinct registers: enabling
        // backup RAM must not disturb the (GSU-owned, read-only) RAM bank.
        c.write(mmio(0x3033), 0x01, 0);
        assert_eq!(c.read(mmio(0x303C), 0), 0x00, "RAMBR unaffected by BRAMR");
    }

    #[test]
    fn reading_sfr_high_byte_clears_irq() {
        let mut c = chip(0, false);
        // Set the STOP IRQ latch (bit 15) via the high-byte write.
        c.write(mmio(0x3031), 0x80, 0);
        assert_eq!(c.read(mmio(0x3031), 0), 0x80, "IRQ latch reads set once");
        // The read cleared it.
        assert_eq!(c.read(mmio(0x3031), 0), 0x00, "IRQ latch cleared on read");
    }

    #[test]
    fn cache_ram_round_trips_and_validates_blocks() {
        let mut c = chip(0, false);
        // Write the whole first 16-byte block; the 16th byte validates it.
        for i in 0..16u16 {
            c.write(mmio(0x3100 + i), 0xA0 + i as u8, 0);
        }
        for i in 0..16u16 {
            assert_eq!(c.read(mmio(0x3100 + i), 0), 0xA0 + i as u8);
        }
    }

    #[test]
    fn dropping_go_flushes_cache_and_resets_cbr() {
        let mut c = chip(0, false);
        // Arm GO via $301F, then clear it through an SFR low-byte write.
        c.write(mmio(0x301F), 0x80, 0);
        assert_ne!(c.read(mmio(0x3030), 0) & 0x20, 0);
        c.write(mmio(0x3030), 0x00, 0); // GO 1->0
        assert_eq!(c.read(mmio(0x3030), 0) & 0x20, 0, "GO cleared");
        // CBR reset to 0 (both bytes read 0).
        assert_eq!(c.read(mmio(0x303E), 0), 0);
        assert_eq!(c.read(mmio(0x303F), 0), 0);
    }

    #[test]
    fn game_pak_ram_round_trips_and_mirrors() {
        let mut c = chip(0x8000, false); // 32 KiB
        c.write(SUPERFX_RAM_BASE, 0x11, 0);
        c.write(SUPERFX_RAM_BASE + 0x7FFF, 0x22, 0);
        assert_eq!(c.read(SUPERFX_RAM_BASE, 0), 0x11);
        assert_eq!(c.read(SUPERFX_RAM_BASE + 0x7FFF, 0), 0x22);
        // One past the end mirrors back to offset 0.
        assert_eq!(c.read(SUPERFX_RAM_BASE + 0x8000, 0), 0x11);
    }

    #[test]
    fn accesses_count_reads_and_writes() {
        let mut c = chip(0x8000, false);
        assert_eq!(c.accesses(), 0);
        c.read(mmio(0x3030), 0);
        c.write(mmio(0x3000), 0x00, 0);
        c.read(SUPERFX_RAM_BASE, 0);
        assert_eq!(c.accesses(), 3);
    }

    #[test]
    fn battery_present_only_with_flag_and_ram() {
        // Battery + RAM -> exposed for `.srm`.
        let mut c = chip(0x8000, true);
        c.write(SUPERFX_RAM_BASE + 0x40, 0x5A, 0);
        assert_eq!(c.battery().map(<[u8]>::len), Some(0x8000));
        assert!(c.battery_dirty(), "a RAM write marks the battery dirty");
        c.clear_battery_dirty();
        assert!(!c.battery_dirty());

        // No battery flag -> RAM works but never persists.
        let mut scratch = chip(0x8000, false);
        scratch.write(SUPERFX_RAM_BASE, 0x99, 0);
        assert_eq!(scratch.read(SUPERFX_RAM_BASE, 0), 0x99);
        assert!(scratch.battery().is_none());

        // No RAM at all -> no battery regardless of the flag.
        assert!(chip(0, true).battery().is_none());
    }

    #[test]
    fn restore_battery_round_trips_ram() {
        let mut c = chip(0x8000, true);
        let mut srm = vec![0u8; 0x8000];
        srm[0x10] = 0xBE;
        srm[0x7FFF] = 0xEF;
        c.restore_battery(&srm);
        assert_eq!(c.read(SUPERFX_RAM_BASE + 0x10, 0), 0xBE);
        assert_eq!(c.read(SUPERFX_RAM_BASE + 0x7FFF, 0), 0xEF);
        assert!(!c.battery_dirty(), "restore clears the dirty flag");
    }

    #[test]
    fn reset_returns_gsu_to_power_on_and_clears_accesses() {
        let mut c = chip(0x8000, true);
        c.write(mmio(0x301F), 0x80, 0); // start GSU
        c.write(mmio(0x3000), 0x77, 0); // dirty R0
        c.write(SUPERFX_RAM_BASE, 0x55, 0); // dirty RAM
        assert!(c.accesses() > 0);

        c.reset(NTSC_HZ);
        assert_eq!(c.accesses(), 0, "accesses cleared");
        assert_eq!(c.read(mmio(0x3030), 0) & 0x20, 0, "GO clear after reset");
        assert_eq!(c.read(mmio(0x3000), 0), 0, "R0 cleared");
        assert_eq!(c.read(mmio(0x303B), 0), 0x04, "VCR = $04 after reset");
        // Game Pak RAM survives the soft reset (static/battery RAM).
        assert_eq!(c.read(SUPERFX_RAM_BASE, 0), 0x55, "RAM survives reset");
    }

    #[test]
    fn savestate_round_trips_gsu_state() {
        let mut c = chip(0x8000, true);
        c.write(mmio(0x3000), 0x34, 0); // R0 low
        c.write(mmio(0x3001), 0x12, 0); // R0 high
        c.write(mmio(0x3034), 0x42, 0); // PBR
        c.write(mmio(0x303A), 0x18, 0); // SCMR
        for i in 0..16u16 {
            c.write(mmio(0x3100 + i), 0xC0 + i as u8, 0); // cache block 0
        }
        let n_before = c.accesses();
        let blob = c.serialize();

        let mut restored = chip(0x8000, true);
        restored.restore(&blob, NTSC_HZ);
        // Check the access counter before the verification reads bump it.
        assert_eq!(restored.accesses(), n_before, "access counter restored");
        assert_eq!(restored.read(mmio(0x3000), 0), 0x34);
        assert_eq!(restored.read(mmio(0x3001), 0), 0x12);
        assert_eq!(restored.read(mmio(0x3034), 0), 0x42);
        assert!(restored.gsu_owns_ram(), "SCMR restored");
        assert_eq!(restored.read(mmio(0x3100), 0), 0xC0, "cache restored");
    }

    #[test]
    fn restore_ignores_garbage_and_foreign_variants() {
        let mut c = chip(0x8000, false);
        c.write(mmio(0x3000), 0xAB, 0);
        // Garbage bytes: dropped, chip untouched.
        c.restore(&[0xFF, 0x00, 0x99], NTSC_HZ);
        assert_eq!(
            c.read(mmio(0x3000), 0),
            0xAB,
            "garbage blob left chip intact"
        );

        // A foreign coprocessor variant is also dropped.
        let foreign = bincode::serialize(&CoprocessorState::Sa1(Default::default())).unwrap();
        c.restore(&foreign, NTSC_HZ);
        assert_eq!(c.read(mmio(0x3000), 0), 0xAB, "foreign variant dropped");
    }

    // -- GSU execution driven by the chip (catch-up / burst) --------------

    #[test]
    fn gsu_runs_a_kernel_and_writes_game_pak_ram() {
        // iwt r1,#$BEEF ; alt2 ; sm ($0010),r1 ; stop, the GSU stores a word
        // to Game Pak RAM, which the S-CPU then reads back through the RAM
        // window (proving the burst ran real code against the real bus).
        let kernel = [0xf1, 0xef, 0xbe, 0x3e, 0xf1, 0x10, 0x00, 0x00];
        let mut c = chip_running_kernel(&kernel, 0x1000, 0x8000);
        arm_and_go(&mut c, 0x0000);
        c.catch_up(100_000);
        assert_eq!(
            c.read(SUPERFX_RAM_BASE + 0x10, 0),
            0xEF,
            "GSU wrote low byte"
        );
        assert_eq!(
            c.read(SUPERFX_RAM_BASE + 0x11, 0),
            0xBE,
            "GSU wrote high byte (drained after stop)"
        );
    }

    #[test]
    fn gsu_plots_a_bitplane_column_into_game_pak_ram() {
        // ibt r0,#$01 ; color ; plot x9 ; stop, the GSU sets COLR = 1 and
        // plots a full 8-pixel column (2bpp), then a 9th plot in the next
        // column flushes column 0 to Game Pak RAM. The S-CPU reads the packed
        // bitplane back, proving the plot engine runs end-to-end through the
        // real `GsuBus`. SCMR stays at md 0 with RON/RAN clear, so the post-
        // stop S-CPU read sees real RAM (not open-bus).
        let kernel = [
            0xa0, 0x01, // ibt r0,#$01
            0x4e, // color   (COLR = color(r0) = $01)
            0x4c, 0x4c, 0x4c, 0x4c, 0x4c, 0x4c, 0x4c, 0x4c, // plot x = 0..7 (fills column 0)
            0x4c, // plot x = 8 (new column flushes column 0)
            0x00, // stop
        ];
        let mut c = chip_running_kernel(&kernel, 0x1000, 0x8000);
        arm_and_go(&mut c, 0x0000);
        c.catch_up(100_000);
        // Every pixel is colour 1 -> plane 0 all set, plane 1 all clear.
        assert_eq!(
            c.read(SUPERFX_RAM_BASE, 0),
            0xFF,
            "plane 0 (colour-1 column)"
        );
        assert_eq!(
            c.read(SUPERFX_RAM_BASE + 1, 0),
            0x00,
            "plane 1 (colour-1 column)"
        );
    }

    #[test]
    fn gsu_getb_reads_real_rom_through_the_buffer_latency() {
        // iwt r14,#$0100 ; getb ; stop, arming R14 starts a ROM-buffer read;
        // getb spends the latency and lands the byte at ROM $0100 in R0.
        let mut rom = vec![0u8; 0x1000];
        rom[..5].copy_from_slice(&[0xfe, 0x00, 0x01, 0xef, 0x00]);
        rom[0x0100] = 0x5A;
        let mut c = SuperFxChip::new(Arc::from(rom.into_boxed_slice()), 0, false, NTSC_HZ);
        arm_and_go(&mut c, 0x0000);
        c.catch_up(100_000);
        assert_eq!(
            read_reg16(&mut c, 0x3000) & 0xFF,
            0x5A,
            "getb read Game Pak ROM $0100 through the ROM buffer"
        );
    }

    #[test]
    fn scpu_sees_ron_open_bus_while_gsu_owns_rom() {
        // Ramp ROM so a real read is distinctive (ROM[$14] = $14).
        let ramp: Vec<u8> = (0..0x1000u32).map(|i| (i & 0xFF) as u8).collect();
        let mut c = SuperFxChip::new(Arc::from(ramp.into_boxed_slice()), 0, false, NTSC_HZ);
        // Idle GSU -> the S-CPU reads the real ROM byte.
        assert_eq!(c.read(SUPERFX_ROM_BASE + 0x14, 0), 0x14, "idle -> real ROM");
        // Arm RON + GO but do not run (master cycle 0): the S-CPU is locked out
        // and reads the fixed open-bus pattern instead.
        c.write(mmio(0x303A), screen_mode::ROM_BUS, 0);
        c.write(mmio(0x301F), 0x00, 0);
        assert_eq!(
            c.read(SUPERFX_ROM_BASE + 0x14, 0),
            RON_OPENBUS[0x14 & 15],
            "RON + running -> open-bus pattern"
        );
        // Clearing RON hands the ROM bus back: real ROM again.
        c.write(mmio(0x303A), 0x00, 0);
        assert_eq!(
            c.read(SUPERFX_ROM_BASE + 0x14, 0),
            0x14,
            "RON clear -> real ROM"
        );
    }

    #[test]
    fn scpu_sees_ran_open_bus_while_gsu_owns_ram() {
        let mut c = chip(0x8000, false);
        c.write(SUPERFX_RAM_BASE + 0x20, 0x5A, 0); // seed RAM (idle write lands)
        assert_eq!(c.read(SUPERFX_RAM_BASE + 0x20, 0), 0x5A, "idle -> real RAM");
        // Arm RAN + GO (do not run), then drive a distinctive value onto the
        // data bus via a RAM write to a different address.
        c.write(mmio(0x303A), screen_mode::RAM_BUS, 0);
        c.write(mmio(0x301F), 0x00, 0);
        c.write(SUPERFX_RAM_BASE + 0x30, 0x99, 0); // last_mdr = 0x99, RAM[$30] = 0x99
        assert_eq!(
            c.read(SUPERFX_RAM_BASE + 0x20, 0),
            0x99,
            "RAN + running -> open-bus MDR, not RAM[$20]"
        );
    }

    #[test]
    fn ram_write_from_scpu_lands_even_under_ran() {
        let mut c = chip(0x8000, false);
        c.write(mmio(0x303A), screen_mode::RAM_BUS, 0);
        c.write(mmio(0x301F), 0x00, 0); // GO + RAN armed
                                        // A RAM write always lands, even under RAN (writes are not gated).
        c.write(SUPERFX_RAM_BASE + 0x30, 0x99, 0);
        // Hand the RAM bus back and confirm the write is in RAM.
        c.write(mmio(0x3030), 0x00, 0); // clear GO
        assert_eq!(
            c.read(SUPERFX_RAM_BASE + 0x30, 0),
            0x99,
            "S-CPU RAM write lands even while the GSU owns the RAM bus"
        );
    }

    #[test]
    fn catch_up_is_monotonic_and_advances_forward() {
        // inc r0 ; bra -3 ; nop, a perpetual loop that increments R0.
        let kernel = [0xd0, 0x05, 0xFD, 0x01];
        let mut c = chip_running_kernel(&kernel, 0x1000, 0);
        arm_and_go(&mut c, 0x0000);
        c.catch_up(1_000);
        let r0_a = read_reg16(&mut c, 0x3000);
        assert!(r0_a > 0, "the burst ran the loop");
        // A catch-up to a past cycle is a no-op.
        c.catch_up(500);
        let r0_b = read_reg16(&mut c, 0x3000);
        assert_eq!(r0_a, r0_b, "catch-up to a past cycle does not advance");
        // A forward catch-up advances the still-running GSU.
        c.catch_up(5_000);
        let r0_c = read_reg16(&mut c, 0x3000);
        assert!(r0_c > r0_a, "a forward catch-up runs more of the loop");
    }

    #[test]
    fn stop_raises_scpu_irq_and_read_3031_clears_it() {
        let mut c = chip_running_kernel(&[0x00], 0x1000, 0); // stop
        arm_and_go(&mut c, 0x0000);
        assert!(!c.has_irq(), "no IRQ before the GSU runs");
        c.catch_up(1_000);
        assert!(c.has_irq(), "an unmasked stop raises the S-CPU IRQ");
        let _ = c.read(mmio(0x3031), 1_001); // reading SFR high byte clears it
        assert!(!c.has_irq(), "reading $3031 deasserts the IRQ line");
    }

    #[test]
    fn masked_stop_does_not_raise_irq() {
        use superfx_gsu::registers::config;
        let mut c = chip_running_kernel(&[0x00], 0x1000, 0); // stop
        c.write(mmio(0x3037), config::IRQ_MASKED, 0); // CFGR masks the STOP interrupt
        arm_and_go(&mut c, 0x0000);
        c.catch_up(1_000);
        assert!(!c.has_irq(), "a masked stop does not raise the IRQ");
    }

    #[test]
    fn disassemble_secondary_decodes_gsu_from_rom() {
        // iwt r1,#$BEEF ; nop.
        let c = chip_running_kernel(&[0xf1, 0xef, 0xbe, 0x01], 0x1000, 0);
        let rows = c.disassemble_secondary(0, 2);
        assert_eq!(rows.len(), 2, "decoded two instructions");
        assert_eq!(rows[0].address, 0);
        assert_eq!(rows[0].mnemonic, "iwt");
        assert!(
            rows[0].operands.to_uppercase().contains("BEEF"),
            "iwt operand carries the 16-bit immediate, got {:?}",
            rows[0].operands
        );
        assert_eq!(rows[1].address, 3, "advanced past the 3-byte iwt");
        assert_eq!(rows[1].mnemonic, "nop");
    }

    #[test]
    fn savestate_round_trips_catch_up_anchor_and_mdr() {
        // A perpetual loop keeps the GSU running (GO stays set) so the restored
        // RAN gate and anchor are both observable.
        let kernel = [0xd0, 0x05, 0xFD, 0x01]; // inc r0 ; bra -3 ; nop
        let mut c = chip_running_kernel(&kernel, 0x1000, 0x8000);
        c.write(mmio(0x303A), screen_mode::RAM_BUS, 0); // SCMR.RAN captured in state
        arm_and_go(&mut c, 0x0000);
        c.catch_up(4_242); // advance the anchor; GSU still running
        c.write(SUPERFX_RAM_BASE + 0x08, 0x7E, 0); // last_mdr = 0x7E (write lands)
        let blob = c.serialize();

        let mut restored = chip_running_kernel(&kernel, 0x1000, 0x8000);
        restored.restore(&blob, NTSC_HZ);
        // The MDR survived: the first (RAN-gated) read returns it, not RAM. No
        // intervening access, so nothing clobbers the restored latch.
        assert_eq!(
            restored.read(SUPERFX_RAM_BASE, 0),
            0x7E,
            "restored MDR feeds the RAN open-bus read"
        );
        // The anchor survived: a catch-up to an earlier cycle is a no-op (R0
        // does not advance).
        let r0_before = read_reg16(&mut restored, 0x3000);
        restored.catch_up(1_000);
        assert_eq!(
            read_reg16(&mut restored, 0x3000),
            r0_before,
            "restored anchor makes a past catch-up a no-op"
        );
    }

    #[test]
    fn savestate_round_trips_the_jump_latch_and_game_pak_ram() {
        // Two fields a complete savestate must carry: the branch-pending latch
        // and the (here non-battery, volatile) Game Pak RAM.
        let mut c = chip(0x8000, false);
        c.gsu.jumped = true;
        c.write(SUPERFX_RAM_BASE + 0x1234, 0xAB, 0);
        let blob = c.serialize();

        let mut restored = chip(0x8000, false);
        restored.restore(&blob, NTSC_HZ);
        assert!(restored.gsu.jumped, "the jump latch round-trips");
        assert_eq!(
            restored.read(SUPERFX_RAM_BASE + 0x1234, 0),
            0xAB,
            "volatile Game Pak RAM round-trips through the savestate (no battery)"
        );
    }

    #[test]
    fn savestate_resave_is_byte_identical_after_plot() {
        // ibt r0,#$01 ; color ; plot x3 ; stop, a partial column leaves the
        // pixel cache with pending bits (never flushed to RAM), so a complete
        // savestate must carry the pixel cache, the register file, the PC, and
        // the halt state. Restoring into a fresh chip that re-serializes to the
        // identical bytes proves every field is captured.
        let kernel = [0xa0, 0x01, 0x4e, 0x4c, 0x4c, 0x4c, 0x00];
        let mut c = chip_running_kernel(&kernel, 0x1000, 0x8000);
        arm_and_go(&mut c, 0x0000);
        c.catch_up(100_000);
        assert_ne!(
            c.gsu.pixel_cache[0].pending, 0,
            "the partial column left pending pixels in the cache"
        );
        let blob = c.serialize();

        let mut restored = chip_running_kernel(&kernel, 0x1000, 0x8000);
        restored.restore(&blob, NTSC_HZ);
        assert_eq!(
            restored.gsu.pixel_cache[0].pending, c.gsu.pixel_cache[0].pending,
            "pixel cache restored"
        );
        assert_eq!(
            restored.serialize(),
            blob,
            "a mid-execution restore re-serializes byte-for-byte"
        );
    }

    #[test]
    fn savestate_lockstep_continues_a_running_gsu() {
        // A perpetual loop keeps GO set; a snapshot mid-loop must resume in
        // lockstep with the original when both run forward to the same cycle.
        let kernel = [0xd0, 0x05, 0xFD, 0x01]; // inc r0 ; bra -3 ; nop
        let mut orig = chip_running_kernel(&kernel, 0x1000, 0);
        arm_and_go(&mut orig, 0x0000);
        orig.catch_up(2_000);
        let blob = orig.serialize();

        let mut restored = chip_running_kernel(&kernel, 0x1000, 0);
        restored.restore(&blob, NTSC_HZ);

        // Run both forward to the same later cycle; the register files stay in
        // lockstep (proving the anchor + running GSU state resumed identically).
        orig.catch_up(8_000);
        restored.catch_up(8_000);
        assert_eq!(
            read_reg16(&mut restored, 0x3000),
            read_reg16(&mut orig, 0x3000),
            "R0 advances identically after a mid-run restore"
        );
    }

    #[test]
    fn debug_state_surfaces_plot_and_config_registers_without_side_effects() {
        let mut c = chip_running_kernel(&[0x00], 0x1000, 0x8000); // stop
        c.write(mmio(0x3038), 0x2A, 0); // SCBR
        c.write(mmio(0x3039), 0x01, 0); // CLSR -> divider = 1
        arm_and_go(&mut c, 0x0000);
        c.catch_up(1_000); // runs the stop -> latches the S-CPU IRQ
        assert!(c.has_irq(), "an unmasked stop latched the S-CPU IRQ");

        let st = c.debug_state();
        let value_of = |name: &str| {
            st.registers
                .iter()
                .find(|r| r.name == name)
                .map(|r| r.value)
        };
        for want in [
            "SCBR", "CFGR", "CLSR", "COLR", "POR", "IRQ", "ROMCL", "ROMDR", "RAMAR", "RAMCL",
            "RAMDR",
        ] {
            assert!(
                value_of(want).is_some(),
                "debug_state is missing the {want} row"
            );
        }
        assert_eq!(value_of("SCBR"), Some(0x2A), "SCBR surfaced");
        assert_eq!(value_of("CLSR"), Some(1), "CLSR divider surfaced");
        assert_eq!(value_of("IRQ"), Some(1), "the pending STOP IRQ surfaced");

        // The generic debug path must never touch the $3031 side effect: reading
        // the state (even twice) leaves the IRQ latched.
        let _ = c.debug_state();
        assert!(
            c.has_irq(),
            "debug_state did not clear the $3031 STOP IRQ latch"
        );
    }

    // -- First-class debugger CPU (secondary_cpu_state / step_secondary_cpu) --

    #[test]
    fn secondary_cpu_state_snapshots_the_gsu_register_file() {
        let mut c = chip(0x8000, false);
        // Seed R0 + PBR through MMIO (no GO side effect at cycle 0).
        c.write(mmio(0x3000), 0x34, 0);
        c.write(mmio(0x3001), 0x12, 0); // R0 = $1234
        c.write(mmio(0x3034), 0x01, 0); // PBR = 1

        let st = c
            .secondary_cpu_state()
            .expect("a Super FX cart exposes the GSU as a secondary CPU");
        // Name contract with debugger-core `COLUMN_SPECS.superfx`: R0..R15 then
        // PBR/ROMBR/RAMBR/CBR/SCBR/SCMR/COLR/POR (24 rows). A drift here breaks
        // the register panel silently, so pin the exact roster.
        let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names.len(), 24, "16 GP regs + 8 control regs");
        assert_eq!(names[0], "R0");
        assert_eq!(names[15], "R15");
        assert_eq!(
            &names[16..24],
            &["PBR", "ROMBR", "RAMBR", "CBR", "SCBR", "SCMR", "COLR", "POR"]
        );
        assert_eq!(st.registers[0].value, 0x1234, "R0 snapshot");
        assert_eq!(st.pc, 0x01_0000, "pc = (PBR<<16)|R15");
        assert_eq!(st.flags, 0, "flags carry SFR (idle at reset)");
    }

    #[test]
    fn step_secondary_cpu_runs_one_gsu_instruction_at_a_time() {
        // A perpetual loop keeps GO set so every step executes an instruction.
        let kernel = [0xd0, 0x05, 0xFD, 0x01]; // inc r0 ; bra -3 ; nop
        let mut c = chip_running_kernel(&kernel, 0x1000, 0);
        arm_and_go(&mut c, 0x0000);
        for _ in 0..12 {
            let step = c
                .step_secondary_cpu()
                .expect("the GSU is a steppable secondary CPU");
            assert!(
                step.instruction_executed,
                "a running GSU executes each step"
            );
            assert!(step.cycles > 0, "an executed step spends clocks");
        }
        assert!(read_reg16(&mut c, 0x3000) > 0, "the loop incremented R0");
    }

    #[test]
    fn step_secondary_cpu_reports_halt_after_stop() {
        let mut c = chip_running_kernel(&[0x00], 0x1000, 0); // stop
        arm_and_go(&mut c, 0x0000);
        // The pipeline is primed with NOP, so a couple of steps run before the
        // `stop` clears GO; find the boundary (bounded) then confirm a halted
        // GSU steps nothing.
        let mut halted = false;
        for _ in 0..8 {
            if !c.step_secondary_cpu().unwrap().instruction_executed {
                halted = true;
                break;
            }
        }
        assert!(halted, "the GSU halts within a few steps after stop");
        let step = c.step_secondary_cpu().unwrap();
        assert!(!step.instruction_executed, "a halted GSU steps nothing");
        assert_eq!(step.cycles, 0, "a halted step spends no clocks");
    }

    // -- GSU instruction-trace capture (inside the catch_up burst) --------

    #[test]
    fn gsu_trace_capture_records_and_formats_entries() {
        use crate::debugger::TRACE_ENTRY_SIZE_GSU;
        use crate::trace_format::format_trace_line_gsu;

        // iwt r0,#$1234 ; iwt r1,#$5678 ; stop, a short linear kernel.
        let kernel = [0xF0, 0x34, 0x12, 0xF1, 0x78, 0x56, 0x00];
        let mut c = chip_running_kernel(&kernel, 0x1000, 0);
        c.set_secondary_trace_enabled(true);
        arm_and_go(&mut c, 0x0000);
        c.catch_up(100_000);

        let raw = c.take_secondary_trace();
        assert!(!raw.is_empty(), "tracing captured GSU instructions");
        assert_eq!(raw.len() % TRACE_ENTRY_SIZE_GSU, 0, "whole records only");
        // Every record parses + formats into a well-formed line, and the real
        // kernel opcode (`iwt`) is decoded, proving the capture is the running
        // program, not just the reset-primed NOP.
        let mut saw_iwt = false;
        for chunk in raw.as_chunks::<TRACE_ENTRY_SIZE_GSU>().0 {
            let entry = TraceEntryBinaryGsu::from_bytes(chunk).unwrap();
            let line = format_trace_line_gsu(&entry);
            assert!(line.contains("R0:"), "line has the R0 column: {line}");
            assert!(line.contains("R15:"), "line has the R15 column: {line}");
            assert!(line.contains("SFR:"), "line has the SFR column: {line}");
            if line.contains("iwt") {
                saw_iwt = true;
            }
        }
        assert!(saw_iwt, "the captured trace decoded the iwt kernel");
    }

    #[test]
    fn disabling_gsu_trace_drops_the_undrained_tail() {
        let kernel = [0xF0, 0x34, 0x12, 0x00]; // iwt r0,#$1234 ; stop
        let mut c = chip_running_kernel(&kernel, 0x1000, 0);
        c.set_secondary_trace_enabled(true);
        arm_and_go(&mut c, 0x0000);
        c.catch_up(100_000);
        // Disabling clears the captured-but-undrained records so a stale tail
        // can't leak into a later trace session.
        c.set_secondary_trace_enabled(false);
        assert!(
            c.take_secondary_trace().is_empty(),
            "disable drops the buffer"
        );
    }
}
