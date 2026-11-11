//! SPC7110 cartridge coprocessor (data decompressor + data-ROM port + ALU +
//! memory-mapping unit, plus the RTC-4513 real-time clock on Tengai Makyou
//! Zero).
//!
//! The SPC7110 (Tengai Makyou Zero, Momotarou Dentetsu Happy,
//! Super Power League 4) is a multi-function enhancement chip. It presents the
//! bus with four MMIO unit windows in `$4800-$4842` plus three routed memory
//! windows, the bank-remapped **MCU** data-ROM window (`$D0-FF`), the
//! **DCU** decompress port (`$50`/`$58`), and the write-gated **SRAM**
//! (`$6000-7FFF`). All of them arrive here as
//! [`MapTarget::ChipMmio`](retro_core::cartridge::MapTarget) dispatches from
//! [`super::presets::spc7110_preset`], split apart by the `base` sentinel the
//! slot carries:
//!
//! - **Registers** (`base = SPC7110_REG_BASE`): the CPU sees `$4800-483F`
//!   (and `$4840-4842` for RTC carts); the chip receives `SPC7110_REG_BASE +
//!   (reg - 0x4800)`.
//! - **SRAM** (`base = SPC7110_SRAM_BASE`): the chip owns the battery buffer
//!   and gates writes on `$4830 bit7`, exactly like bsnes `SPC7110RAM`.
//! - **DCU** (`base = SPC7110_DCU_BASE`): every address in banks `$50`/`$58`
//!   aliases a read of register `$4800` (the decompression FIFO).
//! - **MCU** (`base = SPC7110_MCU_BASE`, `0`): a linear `0..0x2F_FFFF` offset
//!   over the data ROM; the three 1 MiB quadrants remap through the
//!   `dx/ex/fx` bank latches (`$4831/$4832/$4833`).
//!
//! ## Origin and license
//!
//! The module is a translation into Rust of the SPC7110 emulator by byuu
//! and neviksti (version 0.04, 2010), read in the copy bsnes-plus carries
//! under `snes/chip/spc7110/`. That emulator has a notice of its own, on
//! ISC terms, which is reproduced at the end of `LICENSE`. "bsnes" in this
//! module's comments is that code.
//!
//! ## What the module covers
//!
//! It wires the register storage, the `dx/ex/fx` memory-mapping unit (the
//! MCU window remap + `datarom_addr` wrap), the SRAM write-gate + battery,
//! the **ALU** (`$4820-482F`), the **direct data-ROM port** (`$4810-481A`),
//! and the **decompressor** (`$4800-480C`: a `$4806` write reads the directory
//! entry and triggers [`Spc7110Decomp::init`]; `$4800`/DCU reads stream the
//! FIFO and decrement the `$4809/$480A` counter). The **RTC-4513**
//! (`$4840-4842`, `$F9` carts only) is a full state machine in [`rtc`].
//!
//! ## Persistent storage (`battery`)
//!
//! The chip owns a single contiguous `nvram` buffer: `[0..sram_len]` is the
//! write-gated game SRAM and `[sram_len..]` is the 20-byte RTC file (present
//! only on RTC carts, power-on filled with `0xFF` like bsnes `cartrtc`). A
//! single [`Coprocessor::battery`] blob therefore persists both, bsnes keeps
//! them in two files (`.srm` + `.rtc`); we concatenate. The RTC *command
//! machine* ([`Spc7110Rtc`]: state/mode/index/`$484x`) is volatile and rides
//! the savestate instead.
//!
//! ## Timing
//!
//! bsnes models the SPC7110 as zero wait-state (no cycle counting anywhere),
//! so `master_cycle` is accepted but ignored.

use std::sync::Arc;

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};

use crate::savestate::{CoprocessorState, Spc7110State};

mod alu;
mod dataport;
mod decomp;
mod rtc;

use alu::Spc7110Alu;
use dataport::Spc7110DataPort;
use decomp::Spc7110Decomp;
pub use rtc::{fixed_unix_time, set_fixed_unix_time};
use rtc::{RtcClock, Spc7110Rtc, SystemClock, RTC_FILE_LEN};

/// `chip_id` the bus dispatch carries for the SPC7110. Distinct from the
/// DSP-1 (id 1) and S-DD1 (id 5) so a manifest can place each at its own slot.
pub const SPC7110_CHIP_ID: u8 = 6;

/// Chip-address base for the **MCU** data-ROM window (`$D0-FF:0000-FFFF`).
/// The slot hands the chip a linear `0..0x2F_FFFF` offset (three 1 MiB
/// quadrants); zero so the MCU offset is the bare window offset.
pub const SPC7110_MCU_BASE: u32 = 0x0000_0000;

/// Chip-address base for the **DCU** decompress port (`$50`/`$58`). The slot
/// masks the whole bank to a single offset, so every DCU access arrives as
/// exactly `SPC7110_DCU_BASE` and aliases a read of register `$4800`.
pub const SPC7110_DCU_BASE: u32 = 0x1000_0000;

/// Chip-address base for the write-gated **SRAM** (`$6000-7FFF`). The chip
/// owns the battery buffer; a SRAM access arrives as `SPC7110_SRAM_BASE +
/// (index into the buffer)`.
pub const SPC7110_SRAM_BASE: u32 = 0x2000_0000;

/// Chip-address base for the **register** window. A register access arrives as
/// `SPC7110_REG_BASE + (reg - 0x4800)`, so the chip recovers the `$48xx`
/// address by adding `0x4800`. Chosen well above every routed window offset so
/// a single magnitude test splits the four windows apart.
pub const SPC7110_REG_BASE: u32 = 0x3000_0000;

/// Cartridge offset the SPC7110 data ROM starts at (bsnes
/// `cartridge.spc7110_data_rom_offset()`; the shipped carts all use the
/// default). The first 1 MiB is the directly-mapped program ROM; the data ROM
/// the decompressor / data-port / MCU window read lives above it.
const SPC7110_DATA_ROM_OFFSET: u32 = 0x10_0000;

/// Lowest register address ($4800).
const REG_FIRST: u16 = 0x4800;
/// Highest register address ($4842, the RTC status port).
const REG_LAST: u16 = 0x4842;
/// Number of register-array slots ($4800..=$4842 inclusive).
const REG_COUNT: usize = (REG_LAST - REG_FIRST) as usize + 1;

/// Index of `$4830` (the SRAM write-enable + control latch) in the register
/// array.
const R4830: usize = (0x4830 - REG_FIRST) as usize;

/// Index of `$4809` (the decompression output counter low byte).
const R4809: usize = (0x4809 - REG_FIRST) as usize;
/// Index of `$480A` (the decompression output counter high byte).
const R480A: usize = (0x480a - REG_FIRST) as usize;

/// Translate a logical data-ROM address to a physical cartridge offset,
/// wrapping modulo the data-ROM size and re-adding the data-ROM base, bsnes
/// `datarom_addr`. `real_rom_len` is the *unpadded* ROM length (bsnes wraps
/// against the real ROM size, not the power-of-two pad), so the padding tail is
/// never reachable through the data-ROM path.
fn datarom_addr(real_rom_len: usize, addr: u32) -> u32 {
    let size = (real_rom_len as u32).saturating_sub(SPC7110_DATA_ROM_OFFSET);
    if size == 0 {
        return SPC7110_DATA_ROM_OFFSET;
    }
    SPC7110_DATA_ROM_OFFSET + (addr % size)
}

/// Read a byte from the DATA ROM at a *logical* address, wrapping through
/// [`datarom_addr`]. The data-port unit's read ports call this via a closure so
/// the port module stays ROM-agnostic and pure.
fn read_data_rom(rom: &[u8], real_rom_len: usize, logical: u32) -> u8 {
    let phys = datarom_addr(real_rom_len, logical) as usize;
    rom.get(phys).copied().unwrap_or(0)
}

/// The SPC7110 as a [`Coprocessor`].
///
/// Holds the shared cartridge ROM (`Arc<[u8]>`, for the MCU window remap, the
/// decompressor and the data port), the flat `$4800-4842` register
/// array, the three computed `dx/ex/fx` data-ROM window offsets, and the
/// battery-backed SRAM. The directly-mapped program ROM windows live in the
/// hosting [`ComposableCartridge`](super::ComposableCartridge); the chip only
/// serves the routed MCU / DCU / SRAM windows and its own registers.
pub struct Spc7110Chip {
    /// Cartridge ROM image (padded to a power of two), shared with the bus.
    /// The MCU window indexes into it via the `dx/ex/fx` offsets.
    rom: Arc<[u8]>,
    /// The *unpadded* ROM length. bsnes wraps `datarom_addr` modulo
    /// `real_rom_len - data_rom_offset`, so the padding tail is never
    /// addressable through the data-ROM path (it would read zeros).
    real_rom_len: usize,
    /// Flat `$4800-4842` register latches, indexed by `reg - 0x4800`.
    regs: [u8; REG_COUNT],
    /// Data-ROM window offset for the `$D0-DF` MCU quadrant, recomputed on a
    /// `$4831` write as `datarom_addr(r4831 << 20)`.
    dx_offset: u32,
    /// Data-ROM window offset for the `$E0-EF` MCU quadrant (`$4832`).
    ex_offset: u32,
    /// Data-ROM window offset for the `$F0-FF` MCU quadrant (`$4833`).
    fx_offset: u32,
    /// Arithmetic unit (`$4820-$482F`): 16x16 multiply, 32/16 divide.
    alu: Spc7110Alu,
    /// Direct data-ROM access unit (`$4810-$481A`): pointer/adjust/increment
    /// with the `$4818` control matrix.
    dataport: Spc7110DataPort,
    /// Arithmetic decompressor (`$4800-$480C`): a `$4806` write reads the
    /// directory entry and primes it; `$4800` reads stream the output FIFO.
    decomp: Spc7110Decomp,
    /// Battery-backed non-volatile store: `[0..sram_len]` is the game SRAM
    /// (write-gated by `$4830 bit7`); `[sram_len..]` is the 20-byte RTC file
    /// on RTC carts. One buffer so a single `battery()` blob persists both.
    nvram: Vec<u8>,
    /// Split point: `nvram[..sram_len]` is SRAM, `nvram[sram_len..]` is the
    /// RTC file. Also the SRAM bound for [`Self::write_sram`]/[`Self::read_sram`].
    sram_len: usize,
    /// `nvram` changed since the last `.srm` flush (autosave debounce).
    nvram_dirty: bool,
    /// RTC-4513 command machine (`$4840-$4842`), `Some` only on `$F9` carts.
    /// Its 20-byte data file lives in `nvram[sram_len..]`.
    rtc: Option<Spc7110Rtc>,
    /// Host wall-clock source for the RTC (injectable for deterministic tests).
    clock: Arc<dyn RtcClock>,
    /// Whether this cart carries the RTC-4513 (`$F9` header). Governs whether
    /// the RTC ports are live; surfaced in the debug view.
    has_rtc: bool,
    /// Generic chip metadata for the [`Coprocessor::info`] surface.
    info: ExpansionChipInfo,
    /// CPU accesses (reads + writes, every window) since reset, a non-zero
    /// count proves the running game reaches the chip through the bus.
    accesses: u64,
    /// Decompression triggers (`$4806` writes) since reset, a non-zero count
    /// proves the decompressor actually fired on the running ROM. A pure
    /// diagnostic (like the S-DD1 `DECOMPS` counter): reset to 0 on `reset()`
    /// and deliberately left out of the savestate.
    decomp_triggers: u64,
}

impl Spc7110Chip {
    /// Build an SPC7110 over the cartridge ROM.
    ///
    /// `real_rom_len` is the unpadded ROM size (for `datarom_addr` wrapping),
    /// `sram_size` the battery SRAM size in bytes (0 for none), and `has_rtc`
    /// whether the cart carries the RTC-4513. `master_clock_hz` is accepted to
    /// match the [`ChipFactory`](retro_core::cartridge::ChipFactory) seam but
    /// unused (the SPC7110 is zero wait-state and self-clocks nothing except
    /// the RTC, which reads the host clock directly).
    pub fn new(
        rom: Arc<[u8]>,
        real_rom_len: usize,
        sram_size: usize,
        has_rtc: bool,
        _master_clock_hz: u64,
    ) -> Self {
        let mut features = vec![
            "decompress".to_string(),
            "data-port".to_string(),
            "alu".to_string(),
            "mmc".to_string(),
        ];
        if has_rtc {
            features.push("rtc".to_string());
        }
        // One contiguous nvram: SRAM (zeroed) then, on RTC carts, the 20-byte
        // RTC file (power-on filled with 0xFF, matching bsnes `cartrtc`).
        let rtc_len = if has_rtc { RTC_FILE_LEN } else { 0 };
        let mut nvram = vec![0u8; sram_size + rtc_len];
        if has_rtc {
            for byte in nvram[sram_size..].iter_mut() {
                *byte = 0xFF;
            }
        }
        let mut chip = Self {
            rom: Arc::clone(&rom),
            real_rom_len,
            regs: [0u8; REG_COUNT],
            dx_offset: 0,
            ex_offset: 0,
            fx_offset: 0,
            alu: Spc7110Alu::default(),
            dataport: Spc7110DataPort::default(),
            decomp: Spc7110Decomp::new(rom, real_rom_len),
            nvram,
            sram_len: sram_size,
            nvram_dirty: false,
            rtc: has_rtc.then(Spc7110Rtc::default),
            clock: Arc::new(SystemClock),
            has_rtc,
            info: ExpansionChipInfo {
                name: "SPC7110".to_string(),
                chip_type: ExpansionChipType::SPC7110,
                description: "SPC7110 decompressor + data-ROM port + ALU + MMC".to_string(),
                features,
            },
            accesses: 0,
            decomp_triggers: 0,
        };
        chip.init_registers();
        chip
    }

    /// Apply the power-on register defaults. Everything is zero except the
    /// three memory-mapping latches, which bsnes seeds with
    /// `mmio_write(0x4831, 0)`, `mmio_write(0x4832, 1)`, `mmio_write(0x4833,
    /// 2)`, the identity data-ROM window (`$D0-DF` -> first data-ROM MiB,
    /// `$E0-EF` -> second, `$F0-FF` -> third).
    fn init_registers(&mut self) {
        self.regs = [0u8; REG_COUNT];
        self.alu = Spc7110Alu::default();
        self.dataport = Spc7110DataPort::default();
        self.decomp.reset();
        // Reset only the RTC *command machine*; its 20-byte data file in
        // `nvram` is persistent hardware state that survives a console reset.
        if let Some(rtc) = &mut self.rtc {
            rtc.reset();
        }
        self.write_register(0x4831, 0);
        self.write_register(0x4832, 1);
        self.write_register(0x4833, 2);
    }

    /// CPU accesses (reads + writes) to the chip since reset.
    pub fn chip_accesses(&self) -> u64 {
        self.accesses
    }

    /// Translate a logical data-ROM address to a physical cartridge offset,
    /// wrapping modulo the data-ROM size and re-adding the data-ROM base,
    /// bsnes `datarom_addr`. Uses the *unpadded* ROM length so the padding
    /// tail is never reachable (it would read zeros).
    fn datarom_addr(&self, addr: u32) -> u32 {
        datarom_addr(self.real_rom_len, addr)
    }

    /// Read a byte from the cartridge ROM at a *physical* offset (already
    /// resolved through [`Self::datarom_addr`]); out-of-range reads return `0`.
    fn rom_byte(&self, phys: u32) -> u8 {
        self.rom.get(phys as usize).copied().unwrap_or(0)
    }

    /// Read a byte from the MCU data-ROM window. `offset` is the linear
    /// `0..0x2F_FFFF` window offset; the three 1 MiB quadrants pick the
    /// `dx/ex/fx` base (bsnes `SPC7110MCU::read`).
    fn read_mcu(&self, offset: u32) -> u8 {
        let base = match offset >> 20 {
            0 => self.dx_offset,
            1 => self.ex_offset,
            _ => self.fx_offset,
        };
        let addr = base.wrapping_add(offset & 0x0F_FFFF) as usize;
        self.rom.get(addr).copied().unwrap_or(0)
    }

    /// Read a byte from battery SRAM (always allowed; the write gate is on
    /// [`Self::write_sram`]). Bounded by `sram_len` so it never reads into the
    /// RTC file at the tail of `nvram`.
    fn read_sram(&self, index: usize) -> u8 {
        if index >= self.sram_len {
            return 0;
        }
        self.nvram.get(index).copied().unwrap_or(0)
    }

    /// Write a byte to battery SRAM, gated by `$4830 bit7` (bsnes
    /// `SPC7110RAM::write`). A write with the gate closed, or past the SRAM
    /// region into the RTC file, is dropped.
    fn write_sram(&mut self, index: usize, value: u8) {
        if self.regs[R4830] & 0x80 == 0 || index >= self.sram_len {
            return;
        }
        if let Some(cell) = self.nvram.get_mut(index) {
            if *cell != value {
                self.nvram_dirty = true;
            }
            *cell = value;
        }
    }

    /// Dispatch a `$4840`/`$4841` write to the RTC command machine, handing it
    /// the RTC file slice (`nvram[sram_len..]`) and the injected clock, and
    /// marking `nvram` dirty (the write / its `update_time` may mutate the
    /// file). No-op on non-RTC carts.
    fn rtc_write(&mut self, reg: u16, value: u8) {
        let sram_len = self.sram_len;
        let Some(rtc) = &mut self.rtc else {
            return;
        };
        let file = &mut self.nvram[sram_len..];
        let clock = self.clock.as_ref();
        if reg == 0x4840 {
            rtc.write_4840(file, value, clock);
        } else {
            rtc.write_4841(file, value, clock);
        }
        self.nvram_dirty = true;
    }

    /// Read a chip register (`reg` in `$4800-4842`). `$4800` streams the next
    /// decompressed byte and counts down the output counter; the data port
    /// (`$4810-481A`), the ALU (`$4820-482F`) and the RTC (`$4840-4842`)
    /// answer through their units; the decompressor's status port (`$480C`)
    /// returns its latched byte and clears bit 7 (the hardware's
    /// read-to-clear). Every other register reads back its latch.
    fn read_register(&mut self, reg: u16) -> u8 {
        match reg {
            // Decompression FIFO: decrement the 16-bit output counter
            // ($4809/$480A) then stream one decompressed byte. bsnes decrements
            // the counter *before* the FIFO read (it underflows harmlessly when
            // the game reads past the requested length).
            0x4800 => {
                let counter = (u16::from(self.regs[R4809]) | (u16::from(self.regs[R480A]) << 8))
                    .wrapping_sub(1);
                self.regs[R4809] = counter as u8;
                self.regs[R480A] = (counter >> 8) as u8;
                self.decomp.read()
            }
            // Decompression status, bit7 = busy, cleared on read.
            0x480c => self.read_status(0x480c),
            // Data-port read ports: read a data-ROM byte via pointer/adjust and
            // advance per the $4818 matrix.
            0x4810 => {
                let rom = Arc::clone(&self.rom);
                let real_rom_len = self.real_rom_len;
                self.dataport
                    .read_4810(|logical| read_data_rom(&rom, real_rom_len, logical))
            }
            0x481a => {
                let rom = Arc::clone(&self.rom);
                let real_rom_len = self.real_rom_len;
                self.dataport
                    .read_481a(|logical| read_data_rom(&rom, real_rom_len, logical))
            }
            // Data-port latch registers ($4811-$4818).
            0x4811..=0x4818 => self.dataport.read(reg),
            // ALU window: operands, results, mode, and the read-to-clear
            // status port ($482F).
            0x4820..=0x482f => self.alu.read_port(reg),
            // RTC enable latch.
            0x4840 => self.rtc.as_ref().map_or(0, Spc7110Rtc::read_4840),
            // RTC data port: current register, auto-incrementing the index.
            0x4841 => {
                let sram_len = self.sram_len;
                match &mut self.rtc {
                    Some(rtc) => rtc.read_4841(&self.nvram[sram_len..]),
                    None => 0,
                }
            }
            // RTC status, bit7 = ready, cleared on read.
            0x4842 => self.rtc.as_mut().map_or(0, Spc7110Rtc::read_4842),
            // Every other register reads back its stored latch.
            _ => self.load(reg),
        }
    }

    /// Read a status register: return the latched byte, then clear bit 7 (the
    /// hardware clears the "ready/busy" flag on read). A no-op until a unit
    /// sets the flag.
    fn read_status(&mut self, reg: u16) -> u8 {
        let idx = Self::reg_idx(reg);
        let status = self.regs[idx];
        self.regs[idx] &= 0x7f;
        status
    }

    /// Write a chip register (`reg` in `$4800-4842`). The ALU window
    /// (`$4820-482F`) is handled by its own unit (a `$4825`/`$4827` write
    /// triggers the multiply/divide, `$482E` resets it). The memory-mapping
    /// latches (`$4831/$4832/$4833`) recompute the `dx/ex/fx` window offsets;
    /// every other writable register stores its latch. Read-only registers and
    /// the reserved gaps drop the write, matching bsnes `mmio_write` (which has
    /// no `case` for them). A `$4806` write starts a decompression; the
    /// data-port latches (`$4811-4818`) and the RTC ports (`$4840/$4841`) go
    /// to their units.
    fn write_register(&mut self, reg: u16, value: u8) {
        match reg {
            // Decompression trigger: latch $4806, read the 4-byte directory
            // entry (mode + 24-bit BE offset) at datarom_addr(table + index),
            // prime the decoder with the pre-skip, and flag $480C busy.
            0x4806 => {
                self.store(0x4806, value);
                let table = u32::from(self.load(0x4801))
                    | (u32::from(self.load(0x4802)) << 8)
                    | (u32::from(self.load(0x4803)) << 16);
                let index = u32::from(self.load(0x4804)) << 2;
                let addr = self.datarom_addr(table.wrapping_add(index));
                let mode = u32::from(self.rom_byte(addr));
                let offset = (u32::from(self.rom_byte(addr.wrapping_add(1))) << 16)
                    | (u32::from(self.rom_byte(addr.wrapping_add(2))) << 8)
                    | u32::from(self.rom_byte(addr.wrapping_add(3)));
                let preskip =
                    (u32::from(self.load(0x4805)) | (u32::from(value) << 8)).wrapping_shl(mode);
                self.decomp.init(mode, offset, preskip);
                self.store(0x480c, 0x80);
                self.decomp_triggers = self.decomp_triggers.wrapping_add(1);
            }
            // Data-port window: pointer/adjust/increment latches + the $4818
            // control matrix (with its ready-latch + adjust-write side effects).
            0x4811..=0x4818 => self.dataport.write(reg, value),
            // ALU window: operand latches + the multiply/divide/reset triggers.
            0x4820..=0x482f => self.alu.write(reg, value),
            0x4831 => {
                self.store(reg, value);
                self.dx_offset = self.datarom_addr(u32::from(value) << 20);
            }
            0x4832 => {
                self.store(reg, value);
                self.ex_offset = self.datarom_addr(u32::from(value) << 20);
            }
            0x4833 => {
                self.store(reg, value);
                self.fx_offset = self.datarom_addr(u32::from(value) << 20);
            }
            // RTC command machine (enable/latch + index/data port).
            0x4840 | 0x4841 => self.rtc_write(reg, value),
            // Writable latches across the remaining units + the mapping
            // registers $4830/$4834. Trigger side-effects land in their own
            // arms; here we just record the byte so read-back and those
            // triggers see it.
            0x4801..=0x480b | 0x4830 | 0x4834 => self.store(reg, value),
            // $4800, $480C, $4819, $481B-$481F, $4835-$483F, $4842 have no
            // write case in bsnes -> dropped.
            _ => {}
        }
    }

    /// Register-array index for a `$48xx` address.
    fn reg_idx(reg: u16) -> usize {
        (reg - REG_FIRST) as usize
    }

    /// Load a register latch by `$48xx` address.
    fn load(&self, reg: u16) -> u8 {
        self.regs[Self::reg_idx(reg)]
    }

    /// Store a register latch by `$48xx` address.
    fn store(&mut self, reg: u16, value: u8) {
        self.regs[Self::reg_idx(reg)] = value;
    }
}

impl Coprocessor for Spc7110Chip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
        self.accesses += 1;
        if offset >= SPC7110_REG_BASE {
            let reg = REG_FIRST + (offset - SPC7110_REG_BASE) as u16;
            return self.read_register(reg);
        }
        if offset >= SPC7110_SRAM_BASE {
            return self.read_sram((offset - SPC7110_SRAM_BASE) as usize);
        }
        if offset >= SPC7110_DCU_BASE {
            // The DCU decompress port aliases a read of register $4800.
            return self.read_register(0x4800);
        }
        // MCU data-ROM window ($D0-FF), a linear 0..0x2F_FFFF offset.
        self.read_mcu(offset)
    }

    fn write(&mut self, offset: u32, value: u8, _master_cycle: u64) {
        self.accesses += 1;
        if offset >= SPC7110_REG_BASE {
            let reg = REG_FIRST + (offset - SPC7110_REG_BASE) as u16;
            self.write_register(reg, value);
        } else if offset >= SPC7110_SRAM_BASE {
            self.write_sram((offset - SPC7110_SRAM_BASE) as usize, value);
        }
        // The DCU port and the MCU ROM window are read-only; drop writes.
    }

    fn reset(&mut self, _master_clock_hz: u64) {
        // Registers + the dx/ex/fx window return to their power-on defaults.
        // Battery SRAM survives a reset (it is persistent hardware state),
        // exactly what the A/B boot self-test relies on across its resets.
        self.init_registers();
        self.accesses = 0;
        self.decomp_triggers = 0;
    }

    /// Volatile savestate: the typed [`CoprocessorState::Spc7110`] blob, the
    /// flat `$4800-$4842` register array, the resolved `dx/ex/fx` window
    /// offsets, and each functional unit (ALU, data-port, decoder incl. the
    /// mid-decompression decoder state, and, on `$F9` carts, the RTC command
    /// machine). The cartridge ROM and the battery-backed `nvram` (SRAM + the
    /// 20-byte RTC file) are *not* captured, ROM is immutable and the battery
    /// persists via [`Coprocessor::battery`]. The blob is always non-empty (the
    /// mapping latches are meaningful even at reset), so an SPC7110 cartridge
    /// always contributes one snapshot entry.
    fn serialize(&self) -> Vec<u8> {
        let state = CoprocessorState::Spc7110(Spc7110State {
            regs: self.regs.to_vec(),
            dx_offset: self.dx_offset,
            ex_offset: self.ex_offset,
            fx_offset: self.fx_offset,
            alu: self.alu.snapshot(),
            dataport: self.dataport.snapshot(),
            decomp: self.decomp.snapshot(),
            rtc: self.rtc.as_ref().map(Spc7110Rtc::snapshot),
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        // A garbage/empty blob or a foreign variant decodes to something we
        // can't consume and is dropped, leaving the chip untouched.
        let Ok(CoprocessorState::Spc7110(state)) = bincode::deserialize::<CoprocessorState>(data)
        else {
            return;
        };
        let n = self.regs.len().min(state.regs.len());
        self.regs[..n].copy_from_slice(&state.regs[..n]);
        self.dx_offset = state.dx_offset;
        self.ex_offset = state.ex_offset;
        self.fx_offset = state.fx_offset;
        self.alu.restore_state(&state.alu);
        self.dataport.restore_state(&state.dataport);
        // The decoder mutates fields in place, keeping its own ROM `Arc`.
        self.decomp.restore_state(&state.decomp);
        // Restore the RTC command machine only when both sides have one; the
        // persistent 20-byte file in `nvram` is untouched (battery-backed).
        if let (Some(rtc), Some(rtc_state)) = (&mut self.rtc, &state.rtc) {
            rtc.restore_state(rtc_state);
        }
    }

    fn debug_state(&self) -> ExpansionChipState {
        let reg = |address: u32, name: &str, value: u32, description: &str| ChipRegister {
            address,
            name: name.to_string(),
            value,
            description: description.to_string(),
        };
        let mut registers = vec![
            reg(
                0x480c,
                "DEC_STAT",
                u32::from(self.load(0x480c)),
                "Decompress status ($480C, bit7 = busy)",
            ),
            reg(
                0,
                "DEC_MODE",
                self.decomp.mode(),
                "Decompress mode (0/1/2 bpp, 3 = idle)",
            ),
            reg(
                0,
                "DEC_OFF",
                self.decomp.offset(),
                "Decompress DATA-ROM read offset",
            ),
            reg(
                0,
                "DEC_FIFO",
                self.decomp.buffered() as u32,
                "Decompress bytes buffered in the FIFO",
            ),
            reg(
                0,
                "DECOMPS",
                self.decomp_triggers as u32,
                "Decompression triggers ($4806) since reset (truncated)",
            ),
            reg(
                0x4830,
                "R4830",
                u32::from(self.regs[R4830]),
                "SRAM write-enable (bit7) + control",
            ),
            reg(
                0x4831,
                "DX",
                u32::from(self.load(0x4831)),
                "MCU $D0-DF data-ROM bank",
            ),
            reg(
                0x4832,
                "EX",
                u32::from(self.load(0x4832)),
                "MCU $E0-EF data-ROM bank",
            ),
            reg(
                0x4833,
                "FX",
                u32::from(self.load(0x4833)),
                "MCU $F0-FF data-ROM bank",
            ),
            reg(0, "DX_OFF", self.dx_offset, "Resolved $D0-DF ROM offset"),
            reg(0, "EX_OFF", self.ex_offset, "Resolved $E0-EF ROM offset"),
            reg(0, "FX_OFF", self.fx_offset, "Resolved $F0-FF ROM offset"),
            reg(
                0x482e,
                "ALU_MODE",
                u32::from(self.alu.mode()),
                "ALU mode ($482E, bit0 = signed)",
            ),
            reg(
                0x482f,
                "ALU_STAT",
                u32::from(self.alu.status()),
                "ALU status ($482F, bit7 = op complete)",
            ),
            reg(0, "ALU_RES", self.alu.result(), "ALU 32-bit result"),
            reg(
                0,
                "ALU_REM",
                u32::from(self.alu.remainder()),
                "ALU 16-bit remainder",
            ),
            reg(
                0,
                "DPTR",
                self.dataport.pointer(),
                "Data-port 24-bit pointer",
            ),
            reg(
                0,
                "DADJ",
                u32::from(self.dataport.adjust()),
                "Data-port 16-bit adjust",
            ),
            reg(
                0x4818,
                "DCTRL",
                u32::from(self.dataport.control()),
                "Data-port control matrix ($4818)",
            ),
            reg(
                0,
                "DRDY",
                u32::from(self.dataport.ready()),
                "Data-port ready latch (0x07 = armed)",
            ),
            reg(
                0,
                "RTC",
                u32::from(self.has_rtc),
                "RTC-4513 present (1 = yes)",
            ),
            reg(0, "SRAM", self.sram_len as u32, "Battery SRAM size (bytes)"),
            reg(
                0,
                "ACCESSES",
                self.accesses as u32,
                "CPU accesses since reset (truncated)",
            ),
        ];
        if let Some(rtc) = &self.rtc {
            registers.push(reg(
                0x4842,
                "RTC_STAT",
                u32::from(rtc.status()),
                "RTC status ($4842, bit7 = ready)",
            ));
            registers.push(reg(
                0,
                "RTC_STATE",
                u32::from(rtc::state_code(rtc.state())),
                "RTC state (0=Inactive 1=ModeSel 2=IdxSel 3=Write)",
            ));
            registers.push(reg(
                0,
                "RTC_IDX",
                u32::from(rtc.index()),
                "RTC register index (0-15)",
            ));
            registers.push(reg(
                0,
                "RTC_MODE",
                u32::from(rtc::mode_code(rtc.mode())),
                "RTC mode (3=Linear 0x0c=Indexed)",
            ));
            // Current RTC register nibble via the side-effect-free debug read.
            registers.push(reg(
                0x4841,
                "RTC_REG",
                u32::from(rtc.debug_read_4841(&self.nvram[self.sram_len..])),
                "RTC current register nibble ($4841, no side effects)",
            ));
        }
        ExpansionChipState {
            chip_type: ExpansionChipType::SPC7110,
            registers,
            internal_ram: None,
            program_counter: None,
            cycle_count: 0,
        }
    }

    fn label(&self) -> &str {
        "SPC7110"
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }

    fn battery(&self) -> Option<&[u8]> {
        if self.nvram.is_empty() {
            None
        } else {
            Some(&self.nvram)
        }
    }

    fn restore_battery(&mut self, data: &[u8]) {
        let n = data.len().min(self.nvram.len());
        self.nvram[..n].copy_from_slice(&data[..n]);
        self.nvram_dirty = false;
    }

    fn battery_dirty(&self) -> bool {
        self.nvram_dirty
    }

    fn clear_battery_dirty(&mut self) {
        self.nvram_dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_HZ: u64 = 21_477_270;
    /// TMZ shape: 5 MiB real ROM, 8 MiB padded, 8 KiB SRAM, RTC present.
    const TMZ_REAL_LEN: usize = 0x50_0000;
    const TMZ_PADDED_LEN: usize = 0x80_0000;
    const TMZ_SRAM: usize = 8 * 1024;

    /// A padded ROM whose every byte encodes its own offset (low byte), so a
    /// read-back uniquely identifies which physical ROM offset was resolved.
    fn ramp_rom(len: usize) -> Arc<[u8]> {
        let bytes: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
        Arc::from(bytes.into_boxed_slice())
    }

    fn tmz_chip() -> Spc7110Chip {
        Spc7110Chip::new(
            ramp_rom(TMZ_PADDED_LEN),
            TMZ_REAL_LEN,
            TMZ_SRAM,
            true,
            NTSC_HZ,
        )
    }

    /// Read a register through the public [`Coprocessor::read`] path (as the
    /// bus would, after applying the register slot's base).
    fn read_reg(chip: &mut Spc7110Chip, reg: u16) -> u8 {
        chip.read(SPC7110_REG_BASE + u32::from(reg - REG_FIRST), 0)
    }

    /// Write a register through the public [`Coprocessor::write`] path.
    fn write_reg(chip: &mut Spc7110Chip, reg: u16, value: u8) {
        chip.write(SPC7110_REG_BASE + u32::from(reg - REG_FIRST), value, 0);
    }

    fn read_mcu(chip: &mut Spc7110Chip, offset: u32) -> u8 {
        chip.read(SPC7110_MCU_BASE + offset, 0)
    }

    fn read_sram(chip: &mut Spc7110Chip, index: u32) -> u8 {
        chip.read(SPC7110_SRAM_BASE + index, 0)
    }

    fn write_sram(chip: &mut Spc7110Chip, index: u32, value: u8) {
        chip.write(SPC7110_SRAM_BASE + index, value, 0);
    }

    #[test]
    fn label_and_type() {
        let chip = tmz_chip();
        assert_eq!(chip.label(), "SPC7110");
        assert_eq!(chip.info().chip_type, ExpansionChipType::SPC7110);
        assert!(chip.info().features.iter().any(|f| f == "rtc"));
    }

    #[test]
    fn no_rtc_cart_omits_rtc_feature() {
        let chip = Spc7110Chip::new(ramp_rom(0x40_0000), 0x40_0000, TMZ_SRAM, false, NTSC_HZ);
        assert!(!chip.info().features.iter().any(|f| f == "rtc"));
    }

    #[test]
    fn reset_seeds_identity_dx_ex_fx() {
        let chip = tmz_chip();
        // datarom_addr(0) = 0x100000; datarom_addr(0x100000) = 0x200000;
        // datarom_addr(0x200000) = 0x300000 (data-ROM base + bank * 1 MiB).
        assert_eq!(chip.dx_offset, 0x10_0000);
        assert_eq!(chip.ex_offset, 0x20_0000);
        assert_eq!(chip.fx_offset, 0x30_0000);
        // The bank latches read back the raw bank number.
        assert_eq!(chip.load(0x4831), 0);
        assert_eq!(chip.load(0x4832), 1);
        assert_eq!(chip.load(0x4833), 2);
    }

    #[test]
    fn datarom_addr_wraps_modulo_data_rom_size() {
        let chip = tmz_chip();
        // Data-ROM size = real (0x500000) - base (0x100000) = 0x400000.
        // An address of exactly the size wraps back to the base.
        assert_eq!(chip.datarom_addr(0), 0x10_0000);
        assert_eq!(chip.datarom_addr(0x40_0000), 0x10_0000);
        assert_eq!(chip.datarom_addr(0x40_0001), 0x10_0001);
        // A huge bank selector (0xFF << 20) still lands inside the data ROM.
        let a = chip.datarom_addr(0xFF << 20);
        assert!((0x10_0000..0x50_0000).contains(&a), "wrapped addr {a:#x}");
    }

    #[test]
    fn mcu_window_reads_via_dx_ex_fx() {
        let mut chip = tmz_chip();
        let rom = ramp_rom(TMZ_PADDED_LEN);
        // Quadrant 0 ($D0-DF) -> dx_offset (0x100000) + within.
        assert_eq!(read_mcu(&mut chip, 0x00_1234), rom[0x10_1234]);
        // Quadrant 1 ($E0-EF, offset >> 20 == 1) -> ex_offset (0x200000).
        assert_eq!(read_mcu(&mut chip, 0x10_5678), rom[0x20_5678]);
        // Quadrant 2 ($F0-FF, offset >> 20 == 2) -> fx_offset (0x300000).
        assert_eq!(read_mcu(&mut chip, 0x20_ABCD), rom[0x30_ABCD]);
    }

    #[test]
    fn mcu_bank_latch_remaps_quadrant() {
        let mut chip = tmz_chip();
        let rom = ramp_rom(TMZ_PADDED_LEN);
        // Point the $D0-DF quadrant at data-ROM bank 3 -> offset 0x400000.
        write_reg(&mut chip, 0x4831, 3);
        assert_eq!(chip.dx_offset, 0x40_0000);
        assert_eq!(read_mcu(&mut chip, 0x00_0010), rom[0x40_0010]);
        // Read-back returns the raw bank number.
        assert_eq!(read_reg(&mut chip, 0x4831), 3);
    }

    #[test]
    fn sram_write_gate_blocks_when_bit7_clear() {
        let mut chip = tmz_chip();
        // Gate closed by default ($4830 == 0): writes are dropped.
        write_sram(&mut chip, 0x10, 0xAB);
        assert_eq!(read_sram(&mut chip, 0x10), 0);
        assert!(!chip.battery_dirty(), "a dropped write must not dirty SRAM");

        // Open the gate ($4830 bit7) and the write lands.
        write_reg(&mut chip, 0x4830, 0x80);
        write_sram(&mut chip, 0x10, 0xAB);
        assert_eq!(read_sram(&mut chip, 0x10), 0xAB);
        assert!(chip.battery_dirty());
    }

    #[test]
    fn sram_mirrors_are_bounded_by_buffer() {
        let mut chip = tmz_chip();
        write_reg(&mut chip, 0x4830, 0x80);
        write_sram(&mut chip, (TMZ_SRAM - 1) as u32, 0xCD);
        assert_eq!(read_sram(&mut chip, (TMZ_SRAM - 1) as u32), 0xCD);
        // One past the end is out of bounds -> reads 0, write is a no-op.
        write_sram(&mut chip, TMZ_SRAM as u32, 0xEE);
        assert_eq!(read_sram(&mut chip, TMZ_SRAM as u32), 0);
    }

    #[test]
    fn dcu_port_aliases_register_4800() {
        let mut chip = tmz_chip();
        // No decompression has started, so both the register and the DCU port
        // read 0, but crucially the DCU path routes through read_register.
        assert_eq!(chip.read(SPC7110_DCU_BASE, 0), 0);
        assert_eq!(read_reg(&mut chip, 0x4800), 0);
    }

    #[test]
    fn registers_store_and_read_back() {
        let mut chip = tmz_chip();
        // Writable latches across all four units round-trip. ($4806 is *not*
        // here: it is the decompress trigger, tested in the decomp block.
        // $4818 is *not* here either: it is gated by the data-port ready latch;
        // see `dataport_control_write_gated_by_ready`.)
        for (reg, val) in [
            (0x4801_u16, 0x11_u8),
            (0x4802, 0x22),
            (0x4811, 0x33),
            (0x4816, 0x44),
            (0x4820, 0x55),
            (0x482e, 0x66),
            (0x4830, 0x80),
            (0x4834, 0x77),
        ] {
            write_reg(&mut chip, reg, val);
            assert_eq!(read_reg(&mut chip, reg), val, "reg {reg:#06x}");
        }
    }

    #[test]
    fn dataport_control_write_gated_by_ready() {
        let mut chip = tmz_chip();
        // $4818 is inert until all three pointer bytes are loaded.
        write_reg(&mut chip, 0x4818, 0x44);
        assert_eq!(read_reg(&mut chip, 0x4818), 0x00, "ignored before ready");
        write_reg(&mut chip, 0x4811, 0x00);
        write_reg(&mut chip, 0x4812, 0x00);
        write_reg(&mut chip, 0x4813, 0x00);
        write_reg(&mut chip, 0x4818, 0x44);
        assert_eq!(read_reg(&mut chip, 0x4818), 0x44, "accepted once armed");
    }

    #[test]
    fn read_only_registers_drop_writes() {
        let mut chip = tmz_chip();
        // $4800/$480C/$482F have no write case, the write is dropped and the
        // register keeps its (zero) value.
        write_reg(&mut chip, 0x480c, 0xFF);
        write_reg(&mut chip, 0x482f, 0xFF);
        assert_eq!(read_reg(&mut chip, 0x480c), 0);
        assert_eq!(read_reg(&mut chip, 0x482f), 0);
    }

    #[test]
    fn alu_multiply_through_register_window() {
        let mut chip = tmz_chip();
        // Unsigned 16x16: 0x00FF x 0x0010 = 0x0FF0. Operands via $4820/$4821
        // (r1) and $4824/$4825 (r0, high byte triggers).
        write_reg(&mut chip, 0x4820, 0xFF);
        write_reg(&mut chip, 0x4821, 0x00);
        write_reg(&mut chip, 0x4824, 0x10);
        write_reg(&mut chip, 0x4825, 0x00); // trigger
                                            // Status bit7 set.
        assert_eq!(read_reg(&mut chip, 0x482f) & 0x80, 0x80);
        // Result little-endian in $4828-$482B.
        assert_eq!(read_reg(&mut chip, 0x4828), 0xF0);
        assert_eq!(read_reg(&mut chip, 0x4829), 0x0F);
        assert_eq!(read_reg(&mut chip, 0x482a), 0x00);
        assert_eq!(read_reg(&mut chip, 0x482b), 0x00);
    }

    #[test]
    fn alu_divide_through_register_window_and_reset() {
        let mut chip = tmz_chip();
        // Unsigned 100 / 7 = 14 rem 2.
        write_reg(&mut chip, 0x4820, 100);
        write_reg(&mut chip, 0x4821, 0);
        write_reg(&mut chip, 0x4822, 0);
        write_reg(&mut chip, 0x4823, 0);
        write_reg(&mut chip, 0x4826, 7);
        write_reg(&mut chip, 0x4827, 0); // trigger
        assert_eq!(read_reg(&mut chip, 0x4828), 14, "quotient low byte");
        assert_eq!(read_reg(&mut chip, 0x482c), 2, "remainder low byte");
        // $482E reset clears the operand/result latches.
        write_reg(&mut chip, 0x482e, 0);
        assert_eq!(read_reg(&mut chip, 0x4828), 0, "quotient cleared on reset");
        assert_eq!(read_reg(&mut chip, 0x4820), 0, "operand cleared on reset");
    }

    #[test]
    fn dataport_sequential_read_through_register_window() {
        let mut chip = tmz_chip();
        let rom = ramp_rom(TMZ_PADDED_LEN);
        // Point the data port at logical data-ROM address 0 (physical
        // $100000 after datarom_addr) and read sequentially. control = 0 ->
        // increment pointer by 1 each read.
        write_reg(&mut chip, 0x4811, 0x00);
        write_reg(&mut chip, 0x4812, 0x00);
        write_reg(&mut chip, 0x4813, 0x00);
        write_reg(&mut chip, 0x4818, 0x00);
        assert_eq!(read_reg(&mut chip, 0x4810), rom[0x10_0000]);
        assert_eq!(read_reg(&mut chip, 0x4810), rom[0x10_0001]);
        assert_eq!(read_reg(&mut chip, 0x4810), rom[0x10_0002]);
    }

    #[test]
    fn dataport_inert_until_pointer_loaded() {
        let mut chip = tmz_chip();
        // No pointer bytes written -> the read port returns 0.
        assert_eq!(read_reg(&mut chip, 0x4810), 0);
        assert_eq!(read_reg(&mut chip, 0x481a), 0);
    }

    #[test]
    fn dataport_state_clears_on_chip_reset() {
        let mut chip = tmz_chip();
        write_reg(&mut chip, 0x4811, 0x10);
        write_reg(&mut chip, 0x4812, 0x00);
        write_reg(&mut chip, 0x4813, 0x00);
        write_reg(&mut chip, 0x4818, 0x00);
        // Advance the pointer with a read.
        let _ = read_reg(&mut chip, 0x4810);
        chip.reset(NTSC_HZ);
        // Ready latch cleared -> port inert again.
        assert_eq!(read_reg(&mut chip, 0x4810), 0);
    }

    #[test]
    fn alu_state_survives_chip_reset_as_zeroed() {
        let mut chip = tmz_chip();
        write_reg(&mut chip, 0x4820, 0xFF);
        write_reg(&mut chip, 0x4824, 0xFF);
        write_reg(&mut chip, 0x4825, 0x00); // multiply -> nonzero result
        assert_ne!(read_reg(&mut chip, 0x4828), 0);
        chip.reset(NTSC_HZ);
        // bsnes zeroes every ALU register on reset (r4820-r482f).
        assert_eq!(read_reg(&mut chip, 0x4828), 0);
        assert_eq!(read_reg(&mut chip, 0x482f), 0);
    }

    #[test]
    fn status_registers_clear_bit7_on_read() {
        let mut chip = tmz_chip();
        // Force a status latch high via the internal array, then confirm the
        // read returns it and clears bit 7 (read-to-clear).
        chip.regs[Spc7110Chip::reg_idx(0x480c)] = 0x80;
        assert_eq!(read_reg(&mut chip, 0x480c), 0x80);
        assert_eq!(read_reg(&mut chip, 0x480c), 0x00, "bit7 cleared on read");
    }

    #[test]
    fn accesses_count_all_windows_and_reset_clears() {
        let mut chip = tmz_chip();
        assert_eq!(chip.chip_accesses(), 0);
        read_mcu(&mut chip, 0); // MCU read
        let _ = chip.read(SPC7110_DCU_BASE, 0); // DCU read
        read_sram(&mut chip, 0); // SRAM read
        write_reg(&mut chip, 0x4830, 0x80); // register write
        assert_eq!(chip.chip_accesses(), 4);
        assert_eq!(chip.accesses(), 4);

        chip.reset(NTSC_HZ);
        assert_eq!(chip.chip_accesses(), 0);
        // Reset restores the identity dx/ex/fx window.
        assert_eq!(chip.dx_offset, 0x10_0000);
        assert_eq!(chip.ex_offset, 0x20_0000);
        assert_eq!(chip.fx_offset, 0x30_0000);
    }

    #[test]
    fn reset_preserves_battery_sram() {
        let mut chip = tmz_chip();
        write_reg(&mut chip, 0x4830, 0x80);
        write_sram(&mut chip, 0x05, 0x99);
        chip.reset(NTSC_HZ);
        // SRAM survives the reset, the A/B self-test stores its result here
        // and expects it back after the console resets.
        assert_eq!(read_sram(&mut chip, 0x05), 0x99);
        // But the write gate was reset, so a fresh write is blocked again.
        write_sram(&mut chip, 0x06, 0x77);
        assert_eq!(read_sram(&mut chip, 0x06), 0);
    }

    #[test]
    fn battery_round_trips_through_restore() {
        let mut chip = tmz_chip();
        // On an RTC ($F9) cart the battery blob is SRAM + the 20-byte RTC file.
        let total = TMZ_SRAM + RTC_FILE_LEN;
        assert_eq!(chip.battery().map(<[u8]>::len), Some(total));
        let mut save = vec![0u8; total];
        save[0] = 0xDE;
        save[TMZ_SRAM - 1] = 0xAD;
        // The RTC tail persists through the same blob.
        save[TMZ_SRAM] = 0x09;
        chip.restore_battery(&save);
        assert_eq!(read_sram(&mut chip, 0), 0xDE);
        assert_eq!(read_sram(&mut chip, (TMZ_SRAM - 1) as u32), 0xAD);
        // The RTC file byte is not reachable through the SRAM window.
        assert_eq!(read_sram(&mut chip, TMZ_SRAM as u32), 0);
        assert!(!chip.battery_dirty(), "restore clears the dirty flag");
    }

    #[test]
    fn non_rtc_cart_battery_is_sram_only() {
        // A $F5 cart (no RTC) has no 20-byte tail, battery == SRAM exactly.
        let chip = Spc7110Chip::new(ramp_rom(0x40_0000), 0x40_0000, TMZ_SRAM, false, NTSC_HZ);
        assert_eq!(chip.battery().map(<[u8]>::len), Some(TMZ_SRAM));
    }

    #[test]
    fn no_sram_cart_has_no_battery() {
        let chip = Spc7110Chip::new(ramp_rom(0x40_0000), 0x40_0000, 0, false, NTSC_HZ);
        assert!(chip.battery().is_none());
    }

    #[test]
    fn debug_state_surfaces_mapping_and_sram() {
        let mut chip = tmz_chip();
        // Bank 3 maps inside the 5 MiB ROM's 4 MiB data region without
        // wrapping (bank 4 would wrap modulo the 0x400000 data-ROM size).
        write_reg(&mut chip, 0x4831, 3);
        let st = chip.debug_state();
        assert_eq!(st.chip_type, ExpansionChipType::SPC7110);
        let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
        for want in ["R4830", "DX", "DX_OFF", "RTC", "SRAM", "ACCESSES"] {
            assert!(names.contains(&want), "missing debug row {want}");
        }
        let dx = st.registers.iter().find(|r| r.name == "DX").unwrap();
        assert_eq!(dx.value, 3);
        let dx_off = st.registers.iter().find(|r| r.name == "DX_OFF").unwrap();
        assert_eq!(dx_off.value, 0x40_0000, "bank 3 -> data-ROM base + 3 MiB");
        let sram = st.registers.iter().find(|r| r.name == "SRAM").unwrap();
        assert_eq!(sram.value, TMZ_SRAM as u32);
    }

    /// Build a TMZ-shaped padded ROM whose DATA ROM (physical `$100000`)
    /// begins with a single 4-byte directory entry, `{mode, 24-bit BE
    /// offset}` pointing the decoder at logical offset 4, followed by the
    /// compressed `payload`. Returns `(rom, unpadded_len)`.
    fn rom_with_directory(mode: u8, payload: &[u8]) -> (Arc<[u8]>, usize) {
        let base = SPC7110_DATA_ROM_OFFSET as usize;
        let mut data = vec![0u8; 4 + payload.len()];
        data[0] = mode;
        data[1] = 0x00; // offset >> 16
        data[2] = 0x00; // offset >> 8
        data[3] = 0x04; // offset & 0xff -> logical DATA-ROM offset 4
        data[4..].copy_from_slice(payload);

        let real_len = base + data.len();
        let padded_len = real_len.next_power_of_two().max(0x20_0000);
        let mut rom = vec![0u8; padded_len];
        rom[base..base + data.len()].copy_from_slice(&data);
        (Arc::from(rom.into_boxed_slice()), real_len)
    }

    fn sample_payload() -> Vec<u8> {
        (0..96u16)
            .map(|i| (i.wrapping_mul(41) ^ 0x3c) as u8)
            .collect()
    }

    #[test]
    fn decompress_through_4800_matches_standalone_decoder() {
        let payload = sample_payload();
        for mode in 0..3u8 {
            let (rom, real_len) = rom_with_directory(mode, &payload);
            let mut chip = Spc7110Chip::new(Arc::clone(&rom), real_len, TMZ_SRAM, true, NTSC_HZ);
            // table = 0, index = 0, pre-skip = 0 (r4805 = 0, trigger byte 0).
            for reg in [0x4801, 0x4802, 0x4803, 0x4804, 0x4805] {
                write_reg(&mut chip, reg, 0);
            }
            write_reg(&mut chip, 0x4806, 0); // trigger
            let via_chip: Vec<u8> = (0..48).map(|_| read_reg(&mut chip, 0x4800)).collect();

            // A standalone decoder primed with the same directory entry
            // (mode, offset 4, no pre-skip) must produce the identical stream.
            let mut d = decomp::Spc7110Decomp::new(rom, real_len);
            d.init(u32::from(mode), 4, 0);
            let want: Vec<u8> = (0..48).map(|_| d.read()).collect();
            assert_eq!(via_chip, want, "mode {mode} chip path diverged");
        }
    }

    #[test]
    fn trigger_sets_busy_status_bit7() {
        let payload = sample_payload();
        let (rom, real_len) = rom_with_directory(0, &payload);
        let mut chip = Spc7110Chip::new(rom, real_len, TMZ_SRAM, true, NTSC_HZ);
        write_reg(&mut chip, 0x4806, 0); // trigger
                                         // $480C bit7 (busy) is set by the trigger; the debug view sees it
                                         // without the read-to-clear side effect.
        let st = chip.debug_state();
        let stat = st.registers.iter().find(|r| r.name == "DEC_STAT").unwrap();
        assert_eq!(stat.value & 0x80, 0x80, "trigger sets busy");
        // A CPU read of $480C returns 0x80 and clears bit 7.
        assert_eq!(read_reg(&mut chip, 0x480c) & 0x80, 0x80);
        assert_eq!(
            read_reg(&mut chip, 0x480c) & 0x80,
            0x00,
            "bit7 cleared on read"
        );
    }

    #[test]
    fn decomps_counter_increments_per_trigger_and_resets() {
        let payload = sample_payload();
        let (rom, real_len) = rom_with_directory(0, &payload);
        let mut chip = Spc7110Chip::new(rom, real_len, TMZ_SRAM, true, NTSC_HZ);
        let decomps = |c: &Spc7110Chip| {
            c.debug_state()
                .registers
                .iter()
                .find(|r| r.name == "DECOMPS")
                .unwrap()
                .value
        };
        assert_eq!(decomps(&chip), 0);
        write_reg(&mut chip, 0x4806, 0); // trigger #1
        write_reg(&mut chip, 0x4806, 0); // trigger #2
        assert_eq!(decomps(&chip), 2, "one count per $4806 write");
        chip.reset(NTSC_HZ);
        assert_eq!(decomps(&chip), 0, "reset clears the diagnostic counter");
    }

    #[test]
    fn reading_4800_decrements_output_counter() {
        let payload = sample_payload();
        let (rom, real_len) = rom_with_directory(0, &payload);
        let mut chip = Spc7110Chip::new(rom, real_len, TMZ_SRAM, true, NTSC_HZ);
        // Seed the 16-bit output counter to 8 (the trigger leaves it untouched).
        write_reg(&mut chip, 0x4809, 8);
        write_reg(&mut chip, 0x480a, 0);
        write_reg(&mut chip, 0x4806, 0); // trigger
        let _ = read_reg(&mut chip, 0x4800);
        assert_eq!(read_reg(&mut chip, 0x4809), 7);
        let _ = read_reg(&mut chip, 0x4800);
        assert_eq!(read_reg(&mut chip, 0x4809), 6);
    }

    #[test]
    fn dcu_port_streams_same_bytes_as_4800() {
        let payload = sample_payload();

        let (rom_a, len_a) = rom_with_directory(0, &payload);
        let mut chip_a = Spc7110Chip::new(rom_a, len_a, TMZ_SRAM, true, NTSC_HZ);
        write_reg(&mut chip_a, 0x4806, 0);
        let via_dcu: Vec<u8> = (0..24).map(|_| chip_a.read(SPC7110_DCU_BASE, 0)).collect();

        let (rom_b, len_b) = rom_with_directory(0, &payload);
        let mut chip_b = Spc7110Chip::new(rom_b, len_b, TMZ_SRAM, true, NTSC_HZ);
        write_reg(&mut chip_b, 0x4806, 0);
        let via_4800: Vec<u8> = (0..24).map(|_| read_reg(&mut chip_b, 0x4800)).collect();

        assert_eq!(via_dcu, via_4800, "DCU port must alias the $4800 FIFO read");
    }

    #[test]
    fn preskip_via_registers_matches_manual_skip() {
        // r4804 = 0 (index 0), r4805 = 4, trigger byte 0, mode 0 ->
        // pre-skip = (4 | (0<<8)) << 0 = 4. The chip stream must equal a
        // standalone decoder's stream from output index 4.
        let payload = sample_payload();
        let (rom, real_len) = rom_with_directory(0, &payload);
        let mut chip = Spc7110Chip::new(Arc::clone(&rom), real_len, TMZ_SRAM, true, NTSC_HZ);
        write_reg(&mut chip, 0x4805, 4);
        write_reg(&mut chip, 0x4806, 0); // trigger -> pre-skip 4
        let via_chip: Vec<u8> = (0..16).map(|_| read_reg(&mut chip, 0x4800)).collect();

        let mut d = decomp::Spc7110Decomp::new(rom, real_len);
        d.init(0, 4, 0);
        let full: Vec<u8> = (0..20).map(|_| d.read()).collect();
        assert_eq!(
            via_chip,
            &full[4..20],
            "register pre-skip must drop 4 bytes"
        );
    }

    #[test]
    fn decompressor_state_clears_on_chip_reset() {
        let payload = sample_payload();
        let (rom, real_len) = rom_with_directory(0, &payload);
        let mut chip = Spc7110Chip::new(rom, real_len, TMZ_SRAM, true, NTSC_HZ);
        write_reg(&mut chip, 0x4806, 0); // trigger -> mode 0, busy
        let _ = read_reg(&mut chip, 0x4800);
        chip.reset(NTSC_HZ);
        // Reset returns the decoder to mode 3 (idle) -> reads yield 0.
        let st = chip.debug_state();
        let dec_mode = st.registers.iter().find(|r| r.name == "DEC_MODE").unwrap();
        assert_eq!(dec_mode.value, 3, "reset returns decoder to idle mode 3");
        assert_eq!(read_reg(&mut chip, 0x4800), 0);
    }

    #[test]
    fn serialize_emits_a_typed_spc7110_blob() {
        let chip = tmz_chip();
        let blob = chip.serialize();
        assert!(!blob.is_empty(), "SPC7110 always contributes a snapshot");
        assert!(
            matches!(
                bincode::deserialize::<CoprocessorState>(&blob),
                Ok(CoprocessorState::Spc7110(_))
            ),
            "blob decodes to the typed SPC7110 variant"
        );
    }

    #[test]
    fn savestate_round_trips_all_units() {
        let (mut chip, _clock) = tmz_chip_with_clock(1_000_000);
        // Dirty every unit: ALU multiply, data-port pointer, mapping bank,
        // an SRAM byte, and the RTC command machine mid-write.
        write_reg(&mut chip, 0x4820, 0x34);
        write_reg(&mut chip, 0x4821, 0x12);
        write_reg(&mut chip, 0x4824, 0x78);
        write_reg(&mut chip, 0x4825, 0x56); // multiply trigger
        write_reg(&mut chip, 0x4811, 0x11);
        write_reg(&mut chip, 0x4812, 0x22);
        write_reg(&mut chip, 0x4813, 0x01); // pointer armed
        write_reg(&mut chip, 0x4831, 3); // DX bank -> dx_offset
        write_reg(&mut chip, 0x4830, 0x80);
        write_sram(&mut chip, 0x10, 0xAB);
        write_reg(&mut chip, 0x4840, 0x01);
        write_reg(&mut chip, 0x4841, 0x03); // Linear
        write_reg(&mut chip, 0x4841, 0x05); // index 5 -> Write

        let want = chip.serialize();

        // Restore into a fresh chip and re-serialize; the volatile snapshot
        // must be byte-identical.
        let mut fresh = tmz_chip();
        fresh.restore(&want, NTSC_HZ);
        assert_eq!(fresh.serialize(), want, "volatile state round-trips");

        // Spot-check a few observable values.
        assert_eq!(read_reg(&mut fresh, 0x4828), 0x60); // 0x1234*0x5678 low byte
        let st = fresh.debug_state();
        let dx_off = st.registers.iter().find(|r| r.name == "DX_OFF").unwrap();
        assert_eq!(dx_off.value, 0x40_0000, "dx_offset restored");
    }

    #[test]
    fn savestate_round_trips_mid_decompression() {
        // Trigger a real decompression, pull a few bytes, snapshot mid-stream,
        // then prove a restored chip continues the exact same byte sequence,
        // the reason the decoder state lives in struct fields, not statics.
        let payload: Vec<u8> = (0u16..200).map(|i| (i.wrapping_mul(7)) as u8).collect();
        let (rom, real_len) = rom_with_directory(0, &payload);
        let mut chip = Spc7110Chip::new(rom, real_len, TMZ_SRAM, true, NTSC_HZ);

        write_reg(&mut chip, 0x4804, 0); // directory index 0
        write_reg(&mut chip, 0x4806, 0); // trigger
                                         // Pull a handful of bytes so the decoder is mid-stream.
        for _ in 0..10 {
            let _ = read_reg(&mut chip, 0x4800);
        }

        let snapshot = chip.serialize();
        // Continue on the original and capture the reference tail.
        let reference: Vec<u8> = (0..24).map(|_| read_reg(&mut chip, 0x4800)).collect();

        // Restore the snapshot into a fresh chip built on the same ROM and pull
        // the same count, it must match byte-for-byte.
        let (rom2, real_len2) = rom_with_directory(0, &payload);
        let mut restored = Spc7110Chip::new(rom2, real_len2, TMZ_SRAM, true, NTSC_HZ);
        restored.restore(&snapshot, NTSC_HZ);
        let resumed: Vec<u8> = (0..24).map(|_| read_reg(&mut restored, 0x4800)).collect();
        assert_eq!(
            resumed, reference,
            "decoder resumes mid-stream after restore"
        );
    }

    #[test]
    fn restore_ignores_garbage_and_foreign_variants() {
        let mut chip = tmz_chip();
        write_reg(&mut chip, 0x4831, 5); // DX bank 5 -> a non-default dx_offset
        let before = chip.serialize();

        chip.restore(&[], NTSC_HZ); // empty
        chip.restore(&[0xFF, 0x00, 0x13, 0x37], NTSC_HZ); // garbage
        let foreign = bincode::serialize(&CoprocessorState::Sdd1(
            crate::savestate::Sdd1State::default(),
        ))
        .unwrap();
        chip.restore(&foreign, NTSC_HZ); // foreign variant

        assert_eq!(
            chip.serialize(),
            before,
            "bad blobs leave the chip untouched"
        );
    }

    /// Install a deterministic clock so RTC tests can advance wall time.
    fn tmz_chip_with_clock(now: i64) -> (Spc7110Chip, Arc<rtc::FixedClock>) {
        let mut chip = tmz_chip();
        let clock = Arc::new(rtc::FixedClock::new(now));
        chip.clock = clock.clone();
        (chip, clock)
    }

    #[test]
    fn rtc_program_and_indexed_readback_through_registers() {
        let (mut chip, _clock) = tmz_chip_with_clock(1_000_000);

        // Linear write: index 6 (day ones) <- 7.
        write_reg(&mut chip, 0x4840, 0x01); // enable -> ModeSelect
        write_reg(&mut chip, 0x4841, 0x03); // Linear -> IndexSelect
        write_reg(&mut chip, 0x4841, 0x06); // index 6 -> Write
        write_reg(&mut chip, 0x4841, 0x07); // store 7

        // Indexed read of index 6 returns the nibble we stored.
        write_reg(&mut chip, 0x4840, 0x01); // re-arm -> ModeSelect
        write_reg(&mut chip, 0x4841, 0x0c); // Indexed
        write_reg(&mut chip, 0x4841, 0x06); // select index 6
        assert_eq!(read_reg(&mut chip, 0x4841) & 0x0f, 0x07);
    }

    #[test]
    fn rtc_write_lands_in_the_battery_blob_tail() {
        let (mut chip, _clock) = tmz_chip_with_clock(1_000_000);
        write_reg(&mut chip, 0x4840, 0x01);
        write_reg(&mut chip, 0x4841, 0x03); // Linear
        write_reg(&mut chip, 0x4841, 0x02); // index 2 (minute ones)
        write_reg(&mut chip, 0x4841, 0x05); // store 5

        let battery = chip.battery().expect("RTC cart has a battery");
        assert_eq!(battery.len(), TMZ_SRAM + RTC_FILE_LEN);
        // The RTC file is the 20-byte tail; index 2 within it.
        assert_eq!(battery[TMZ_SRAM + 2] & 0x0f, 0x05);
        assert!(chip.battery_dirty(), "an RTC write marks nvram dirty");
    }

    #[test]
    fn rtc_status_bit7_clears_on_read_through_registers() {
        let (mut chip, _clock) = tmz_chip_with_clock(1_000_000);
        write_reg(&mut chip, 0x4840, 0x01); // enable sets ready
        assert_eq!(read_reg(&mut chip, 0x4842) & 0x80, 0x80);
        assert_eq!(read_reg(&mut chip, 0x4842) & 0x80, 0, "read-to-clear");
    }

    #[test]
    fn rtc_advances_across_clock_ticks_through_registers() {
        let (mut chip, clock) = tmz_chip_with_clock(1_000_000);

        // Program 2000-01-01 00:00:00 (weekday 6) + zeroed CR0/CR2 (enabled).
        write_reg(&mut chip, 0x4840, 0x01);
        write_reg(&mut chip, 0x4841, 0x03); // Linear
        write_reg(&mut chip, 0x4841, 0x00); // index 0 -> Write
        for nibble in [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 6, 0, 0, 0] {
            write_reg(&mut chip, 0x4841, nibble);
        }
        write_reg(&mut chip, 0x4840, 0x00); // disable -> sync timestamp
        clock.advance(65);
        write_reg(&mut chip, 0x4840, 0x00); // disable -> fold in 65s

        // Indexed read of seconds/minutes.
        write_reg(&mut chip, 0x4840, 0x01);
        write_reg(&mut chip, 0x4841, 0x0c); // Indexed
        write_reg(&mut chip, 0x4841, 0x00); // select index 0
        assert_eq!(
            read_reg(&mut chip, 0x4841) & 0x0f,
            5,
            "seconds ones (65s -> 1:05)"
        );
        assert_eq!(read_reg(&mut chip, 0x4841) & 0x0f, 0, "seconds tens");
        assert_eq!(read_reg(&mut chip, 0x4841) & 0x0f, 1, "minutes ones");
    }

    #[test]
    fn reset_clears_rtc_machine_but_keeps_the_data_file() {
        let (mut chip, _clock) = tmz_chip_with_clock(1_000_000);
        // Write hour ones (index 4) <- 9 and leave the machine mid-Write.
        write_reg(&mut chip, 0x4840, 0x01);
        write_reg(&mut chip, 0x4841, 0x03); // Linear
        write_reg(&mut chip, 0x4841, 0x04); // index 4 -> Write
        write_reg(&mut chip, 0x4841, 0x09); // store 9

        chip.reset(NTSC_HZ);

        // The 20-byte data file survives the reset...
        let battery = chip.battery().unwrap();
        assert_eq!(battery[TMZ_SRAM + 4] & 0x0f, 0x09);
        // ...but the command machine is back to Inactive (a bare read is inert).
        assert_eq!(read_reg(&mut chip, 0x4841), 0);
        let st = chip.debug_state();
        let state = st.registers.iter().find(|r| r.name == "RTC_STATE").unwrap();
        assert_eq!(state.value, 0, "reset -> Inactive");
    }

    #[test]
    fn non_rtc_cart_rtc_ports_are_inert() {
        // A $F5 cart has no RTC, the ports read back 0 and writes are no-ops.
        let mut chip = Spc7110Chip::new(ramp_rom(0x40_0000), 0x40_0000, TMZ_SRAM, false, NTSC_HZ);
        write_reg(&mut chip, 0x4840, 0x01);
        write_reg(&mut chip, 0x4841, 0x03);
        assert_eq!(read_reg(&mut chip, 0x4840), 0);
        assert_eq!(read_reg(&mut chip, 0x4841), 0);
        assert_eq!(read_reg(&mut chip, 0x4842), 0);
        // No RTC debug rows on a non-RTC cart.
        let st = chip.debug_state();
        assert!(!st.registers.iter().any(|r| r.name == "RTC_STATE"));
    }
}
