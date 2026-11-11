//! The SA-1 (Super Mario RPG, Kirby Super Star, Kirby's Dream Land 3,
//! Marvelous, Jikkyou Oshaberi Parodius, PGA Tour 96): a second 65816 on the
//! cartridge, on the console's master clock, with a mapper for ROM (the
//! Super MMC), 2 KiB of I-RAM, up to 256 KiB of BW-RAM shared with the S-CPU,
//! an arithmetic unit ([`math`]), a variable-length bit reader ([`varbit`]),
//! an H/V timer ([`timer`]), DMA with character conversion ([`dma`]), and
//! interrupts both ways ([`interrupts`]).
//!
//! ## Running the second CPU
//!
//! The SA-1's CPU is the `wdc65816` crate's [`Wdc65816`], run in bursts
//! against its own bus ([`Sa1Bus`]): whenever the S-CPU touches the chip,
//! and once per S-CPU instruction (`system.rs` does it for cartridges with a
//! CPU of their own).
//! [`Sa1Chip::sa1_master`] counts the master cycles the SA-1 has used, and a
//! burst runs it up to the S-CPU's. For the burst the CPU is taken out of the
//! chip, so the bus can borrow the rest. The S-CPU starts the SA-1 by
//! releasing its reset through CCNT (`$2200`); that write can arrive while
//! the CPU is out, so the start waits for the next burst
//! ([`Sa1Chip::pending_reset`]).
//!
//! The SA-1 takes its interrupts from vectors held in registers (CNV, CIV),
//! not from the top of bank `$00`. Before each instruction the burst hands
//! the CPU the SA-1's IRQ line and the NMI's rise, and for the instruction
//! that takes an interrupt the bus answers the vector fetch from the
//! registers ([`Sa1Chip::vector_redirect`]); a data read of the same address
//! still reads ROM. The CPU's own rules then apply: NMI before IRQ, the I
//! flag, waking from WAI.
//!
//! ## One chip, five windows
//!
//! The chip answers on five windows of the S-CPU's bus, routed here by
//! [`super::presets::sa1_preset`] with a base that tells them apart:
//!
//! - **Registers** (`SA1_MMIO_BASE`): `$2200-$23FF`, the bank folded away.
//! - **I-RAM** (`SA1_IRAM_BASE`): `$3000-$37FF`, every bank the same 2 KiB.
//! - **BW-RAM window** (`SA1_BWRAM_BASE`): `$6000-$7FFF`, the 8 KiB block
//!   BMAPS (`$2224`) chooses.
//! - **BW-RAM** (`SA1_BWRAM_LINEAR_BASE`): banks `$40-$4F`, all of it in
//!   order.
//! - **ROM** (`SA1_ROM_BASE` plus the window's start): the Super MMC's
//!   windows. The base carries the window's start, so the chip sees the full
//!   24-bit address and maps it itself.
//!
//! ROM and both RAMs come through the chip, rather than straight from the
//! cartridge's memory, because the Super MMC maps ROM as the program changes
//! it, and because each S-CPU access must first bring the SA-1 up to the
//! same moment.
//!
//! Behaviour checked against bsnes-plus and ares.

use std::sync::Arc;

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};
use retro_core::cpu::Addressable;
use retro_core::emulator::StepResult;
use retro_core::types::{CpuState, Instruction};
use wdc65816::instructions::FLAG_I;
use wdc65816::Wdc65816;

use crate::debugger::cpu_state_65816;
use crate::savestate::{CoprocessorState, Cpu65816State, Sa1State};

mod bitmap;
mod bus;
mod dma;
mod interrupts;
mod math;
mod registers;
mod timer;
mod varbit;

use bus::Sa1Bus;
use dma::{Sa1Dma, Source, Start};
use interrupts::Interrupts;
use math::Sa1Math;
use registers::{field, flag, Bytes};
use timer::Sa1Timer;
use varbit::Sa1VarBit;

pub use crate::cartridge::chip_ids::{
    SA1_BWRAM_BASE, SA1_BWRAM_LINEAR_BASE, SA1_IRAM_BASE, SA1_MMIO_BASE, SA1_ROM_BASE,
};

/// I-RAM: 2 KiB.
const IRAM_LEN: usize = 2048;

/// The lines a frame has in the console's region, told by its master clock
/// (NTSC 21.477 MHz: 262 lines; PAL 21.281 MHz: 312), for the SA-1's H/V
/// timer, which counts the console's frame. The split lies between the two
/// clocks, so a clock that runs a little off still finds its region.
fn region_lines(master_clock_hz: u64) -> u16 {
    if master_clock_hz < 21_400_000 {
        312
    } else {
        262
    }
}

/// The SA-1, as a [`Coprocessor`].
pub struct Sa1Chip {
    /// The cartridge ROM (its size a power of two), mapped by the Super MMC;
    /// an offset past its end reads it again from the start.
    rom: Arc<[u8]>,
    /// I-RAM: `$3000-$37FF` for the S-CPU, also `$0000-$07FF` for the SA-1.
    iram: [u8; IRAM_LEN],
    /// BW-RAM, 0 to 256 KiB; kept by a battery on some cartridges
    /// ([`Sa1Chip::has_battery`]).
    bwram: Vec<u8>,
    /// CXB-FXB (`$2220-$2223`) bits 0-2: the 1 MiB block of ROM (of 8) each
    /// of the four ROM windows, C, D, E and F, shows.
    rom_block: [u8; 4],
    /// CXB-FXB bit 7: the window's banks at `$8000-$FFFF` show the chosen
    /// block too. While clear they show the window's own megabyte (C the
    /// first, D the second, E the third, F the fourth), 32 KiB a bank. The
    /// `$C0-$FF` banks always show the chosen block, 64 KiB a bank.
    rom_block_mode: [bool; 4],
    /// BMAPS (`$2224`): the 8 KiB block of BW-RAM the S-CPU sees at
    /// `$6000-$7FFF`.
    scpu_bwram_block: u8,
    /// BMAP (`$2225`) bits 0-6: the block the SA-1 sees at `$6000-$7FFF`.
    sa1_bwram_block: u8,
    /// BMAP bit 7: the SA-1's window shows BW-RAM as a bitmap, 2 or 4 bits a
    /// pixel (BBF, [`bitmap`]), rather than as bytes. The S-CPU's views are
    /// not affected.
    bwram_bitmap: bool,
    /// SBWE, CBWE, BWPA, SIWP and CIWP (`$2226-$222A`): write protection of
    /// BW-RAM and I-RAM. Kept, not applied.
    write_protection: [u8; 5],
    /// The arithmetic unit (`$2250-$2254` written, `$2306-$230B` read).
    math: Sa1Math,
    /// The SA-1's CPU, taken out of the chip while a burst runs it (see
    /// [`Coprocessor::catch_up`]). Starts held in reset.
    cpu: Wdc65816,
    /// The master cycles the SA-1 has used, the point a burst runs on from.
    /// The SA-1 runs on the console's master clock, so the S-CPU's cycle
    /// count needs no conversion.
    sa1_master: u64,
    /// CCNT (`$2200`) bit 5: the SA-1's CPU is held in reset. Set at
    /// power-on; the S-CPU starts the SA-1 by clearing it.
    held_in_reset: bool,
    /// CCNT bit 6: the SA-1's CPU is paused.
    paused: bool,
    /// CRV (`$2203-$2204`): where the SA-1's CPU starts.
    reset_vector: u16,
    /// A start the S-CPU asked for, waiting for the next burst: the CPU
    /// begins at this address in bank `$00`.
    pending_reset: Option<u16>,
    /// The last byte on the SA-1's data bus, which an address that maps to
    /// nothing reads back; also what a copy reads from such an address.
    cpu_mdr: u8,
    /// The H/V timer (`$2210-$2215` written, `$2302-$2305` read).
    timer: Sa1Timer,
    /// DMA (`$2230-$224F`): copies and the two character conversions.
    dma: Sa1Dma,
    /// The variable-length bit reader (`$2258-$225B` written, `$230C-$230D`
    /// read), which reads its stream through [`Sa1Chip::vbr_read`].
    varbit: Sa1VarBit,
    /// Interrupts both ways, their vectors, and the line to the S-CPU.
    interrupts: Interrupts,
    /// Set only for the instruction that takes an interrupt: the bus then
    /// answers the vector fetch from CNV or CIV. Never saved.
    vector_redirect: bool,
    /// The cartridge keeps BW-RAM with a battery (header `$34`/`$35`); a
    /// cartridge without one must not save it.
    has_battery: bool,
    /// The chip's description for the [`Coprocessor::info`] surface.
    info: ExpansionChipInfo,
    /// S-CPU accesses to the chip since reset, through every window.
    accesses: u64,
    /// BW-RAM changed since it was last saved.
    bwram_dirty: bool,
}

impl Sa1Chip {
    /// An SA-1 over the cartridge's ROM, with `bwram_size` bytes of BW-RAM
    /// (0 for none), kept by a battery when `has_battery`. The master clock
    /// tells the timer the region's lines per frame ([`region_lines`]). The
    /// Super MMC starts with the first four megabytes in order.
    pub fn new(rom: Arc<[u8]>, bwram_size: usize, has_battery: bool, master_clock_hz: u64) -> Self {
        let mut features = vec!["mmc".to_string(), "iram".to_string()];
        if bwram_size > 0 {
            features.push("bwram".to_string());
        }
        let mut timer = Sa1Timer::default();
        timer.set_lines_per_frame(region_lines(master_clock_hz));
        Self {
            rom,
            iram: [0u8; IRAM_LEN],
            bwram: vec![0u8; bwram_size],
            rom_block: [0, 1, 2, 3],
            rom_block_mode: [false; 4],
            scpu_bwram_block: 0,
            sa1_bwram_block: 0,
            bwram_bitmap: false,
            write_protection: [0; 5],
            math: Sa1Math::default(),
            cpu: Wdc65816::new(),
            sa1_master: 0,
            held_in_reset: true,
            paused: false,
            reset_vector: 0,
            pending_reset: None,
            cpu_mdr: 0,
            timer,
            dma: Sa1Dma::default(),
            varbit: Sa1VarBit::default(),
            interrupts: Interrupts::default(),
            vector_redirect: false,
            has_battery,
            info: ExpansionChipInfo {
                name: "SA-1".to_string(),
                chip_type: ExpansionChipType::SA1,
                description: "SA-1 Super MMC + I-RAM + BW-RAM + 65816 + timer/IRQ/DMA".to_string(),
                features,
            },
            accesses: 0,
            bwram_dirty: false,
        }
    }

    /// S-CPU accesses to the chip since reset.
    pub fn chip_accesses(&self) -> u64 {
        self.accesses
    }

    /// A ROM byte; an offset past the end reads the ROM again from the
    /// start.
    fn read_rom(&self, offset: u32) -> u8 {
        if self.rom.is_empty() {
            return 0;
        }
        self.rom[(offset as usize) % self.rom.len()]
    }

    /// A ROM byte at a 24-bit address, through the Super MMC. In the system
    /// banks (`$00-$3F`, `$80-$BF`) each window's `$8000-$FFFF` takes 32 KiB
    /// a bank; the `$C0-$FF` banks take 64 KiB a bank of the chosen block.
    /// Banks `$40-$7F` hold no ROM and read as zero here.
    fn rom_via_mmc(&self, snes_addr: u32) -> u8 {
        let bank = (snes_addr >> 16) as u8;
        let offset = snes_addr as u16;
        let rom_offset = match bank {
            0x00..=0x3F | 0x80..=0xBF => {
                let window = usize::from(((bank >> 5) & 1) | ((bank >> 6) & 2));
                let bank_in_window = u32::from(bank & 0x1F);
                self.window_megabyte(window) + bank_in_window * 0x8000 + u32::from(offset & 0x7FFF)
            }
            0xC0..=0xFF => {
                let window = usize::from((bank >> 4) & 3);
                (u32::from(self.rom_block[window]) << 20)
                    + u32::from(bank & 0x0F) * 0x1_0000
                    + u32::from(offset)
            }
            _ => return 0,
        };
        self.read_rom(rom_offset)
    }

    /// Where a window's banks at `$8000-$FFFF` start in ROM: the chosen block
    /// when its mode bit is set, else the window's own megabyte.
    fn window_megabyte(&self, window: usize) -> u32 {
        let block = if self.rom_block_mode[window] {
            self.rom_block[window]
        } else {
            window as u8
        };
        u32::from(block) << 20
    }

    /// An I-RAM byte (the offset is folded into the 2 KiB).
    fn iram_read(&self, rel: u32) -> u8 {
        self.iram[(rel as usize) & (IRAM_LEN - 1)]
    }

    /// Write an I-RAM byte. The write protection is not applied.
    fn iram_write(&mut self, rel: u32, value: u8) {
        self.iram[(rel as usize) & (IRAM_LEN - 1)] = value;
    }

    /// A byte of the S-CPU's 8 KiB BW-RAM window (the BMAPS block).
    fn bwram_window_read(&self, rel: u32) -> u8 {
        if self.bwram.is_empty() {
            return 0;
        }
        let addr = usize::from(self.scpu_bwram_block) * 0x2000 + (rel as usize & 0x1FFF);
        self.bwram[addr % self.bwram.len()]
    }

    /// Write a byte through the S-CPU's window. The write enable is not
    /// applied.
    fn bwram_window_write(&mut self, rel: u32, value: u8) {
        if self.bwram.is_empty() {
            return;
        }
        let len = self.bwram.len();
        let addr = (usize::from(self.scpu_bwram_block) * 0x2000 + (rel as usize & 0x1FFF)) % len;
        self.store_bwram(addr, value);
    }

    /// A byte of the SA-1's 8 KiB window at `$6000-$7FFF`. As bytes, it
    /// shows the BMAP block (its low five bits); as a bitmap (BMAP bit 7),
    /// the window's addresses are pixels counted from the BMAP block, two or
    /// four to a byte by BBF.
    fn bwram_window_sa1_read(&self, rel: u32) -> u8 {
        if self.bwram.is_empty() {
            return 0;
        }
        let window = rel & 0x1FFF;
        if self.bwram_bitmap {
            let pixel = u32::from(self.sa1_bwram_block) * 0x2000 + window;
            bitmap::bitmap_read(&self.bwram, pixel, self.dma.bitmap_2bpp())
        } else {
            let addr = usize::from(self.sa1_bwram_block & 0x1F) * 0x2000 + window as usize;
            self.bwram[addr % self.bwram.len()]
        }
    }

    /// Write a byte through the SA-1's window, as bytes or as a bitmap like
    /// [`Self::bwram_window_sa1_read`]; a pixel keeps the rest of its byte.
    fn bwram_window_sa1_write(&mut self, rel: u32, value: u8) {
        if self.bwram.is_empty() {
            return;
        }
        let window = rel & 0x1FFF;
        if self.bwram_bitmap {
            let pixel = u32::from(self.sa1_bwram_block) * 0x2000 + window;
            if bitmap::bitmap_write(&mut self.bwram, pixel, value, self.dma.bitmap_2bpp()) {
                self.bwram_dirty = true;
            }
        } else {
            let len = self.bwram.len();
            let addr = (usize::from(self.sa1_bwram_block & 0x1F) * 0x2000 + window as usize) % len;
            self.store_bwram(addr, value);
        }
    }

    /// A byte of BW-RAM in order (banks `$40-$4F`).
    fn bwram_linear_read(&self, rel: u32) -> u8 {
        if self.bwram.is_empty() {
            return 0;
        }
        self.bwram[(rel as usize) % self.bwram.len()]
    }

    /// Write a byte of BW-RAM in order.
    fn bwram_linear_write(&mut self, rel: u32, value: u8) {
        if self.bwram.is_empty() {
            return;
        }
        let len = self.bwram.len();
        let addr = (rel as usize) % len;
        self.store_bwram(addr, value);
    }

    /// Store a BW-RAM byte, marking BW-RAM as needing a save only when the
    /// byte changes.
    fn store_bwram(&mut self, index: usize, value: u8) {
        if let Some(cell) = self.bwram.get_mut(index) {
            if *cell != value {
                self.bwram_dirty = true;
            }
            *cell = value;
        }
    }

    /// A read of a register (`$2200-$23FF`). Two have an effect besides the
    /// answer: `$2302` copies the timer's counters into HCR and VCR, and
    /// `$230D` moves the bit reader on when VBD says so. The debugger never
    /// comes here, it reads the counters and the cursor themselves
    /// ([`Coprocessor::debug_state`]). The registers written by the program,
    /// `$2200-$22FF`, read as zero.
    fn mmio_read(&mut self, reg: u16) -> u8 {
        match reg {
            0x2300 => self.interrupts.scpu_flags(),
            0x2301 => self.interrupts.sa1_flags(),
            0x2302 => {
                self.timer.latch();
                self.timer.h_latched().byte(0)
            }
            0x2303 => self.timer.h_latched().byte(1),
            0x2304 | 0x2305 => self.timer.v_latched().byte(usize::from(reg - 0x2304)),
            0x2306..=0x230B => self.math.read(reg),
            0x230C => self.varbit_field().byte(0),
            0x230D => {
                let field = self.varbit_field();
                if self.varbit.advances_on_read() {
                    self.varbit.advance();
                }
                field.byte(1)
            }
            // VC, the chip's version. What the chip returns is not known.
            0x230E => 0x01,
            _ => 0,
        }
    }

    /// The 16 bits at the bit reader's cursor, from the three bytes at its
    /// address read through [`Self::vbr_read`]. It changes nothing.
    fn varbit_field(&self) -> u16 {
        let address = self.varbit.address();
        let byte = |step: u32| self.vbr_read(address.wrapping_add(step));
        self.varbit.field([byte(0), byte(1), byte(2)])
    }

    /// The bit reader's own view of memory, read-only: ROM through the Super
    /// MMC, I-RAM (`$0000-$07FF`, `$3000-$37FF`), and BW-RAM as bytes in
    /// order (the `$6000-$7FFF` of each system bank taking the next 8 KiB,
    /// and banks `$40-$4F`). It reaches no register, the SA-1's windowed or
    /// bitmap BW-RAM, or the S-CPU's side, so a stream can never set off a
    /// register's effect or wait for the S-CPU. Anything else reads the open
    /// bus.
    fn vbr_read(&self, addr: u32) -> u8 {
        let addr = addr & 0xFF_FFFF;
        let bank = (addr >> 16) as u8;
        let offset = addr as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match offset {
                0x0000..=0x07FF | 0x3000..=0x37FF => self.iram_read(u32::from(offset)),
                0x8000..=0xFFFF => self.rom_via_mmc(addr),
                0x6000..=0x7FFF if !self.bwram.is_empty() => {
                    let block = u32::from(bank & 0x3F);
                    self.bwram_linear_read(block * 0x2000 + u32::from(offset - 0x6000))
                }
                _ => self.cpu_mdr,
            },
            0x40..=0x4F if !self.bwram.is_empty() => self.bwram_linear_read(addr - 0x40_0000),
            0xC0..=0xFF => self.rom_via_mmc(addr),
            _ => self.cpu_mdr,
        }
    }

    /// A write of a register (`$2200-$23FF`). Writes of `$2300-$23FF`, the
    /// registers the program reads, do nothing.
    fn mmio_write(&mut self, reg: u16, value: u8) {
        let at = |first: u16| usize::from(reg - first);
        match reg {
            0x2200 => self.write_reset_control(value),
            0x2201 => self.interrupts.write_scpu_enable(value),
            0x2202 => self.interrupts.write_scpu_acknowledge(value),
            0x2203 | 0x2204 => self.reset_vector = self.reset_vector.with_byte(at(0x2203), value),
            0x2205 | 0x2206 => self.interrupts.write_nmi_vector(at(0x2205), value),
            0x2207 | 0x2208 => self.interrupts.write_irq_vector(at(0x2207), value),
            0x2209 => self.interrupts.write_sa1_control(value),
            0x220A => self.interrupts.write_sa1_enable(value),
            0x220B => self.interrupts.write_sa1_acknowledge(value),
            0x220C | 0x220D => self.interrupts.write_scpu_nmi_vector(at(0x220C), value),
            0x220E | 0x220F => self.interrupts.write_scpu_irq_vector(at(0x220E), value),
            0x2210 => self.timer.write_control(value),
            0x2211 => self.timer.restart(),
            0x2212 | 0x2213 => self.timer.write_h_target(at(0x2212), value),
            0x2214 | 0x2215 => self.timer.write_v_target(at(0x2214), value),
            0x2220..=0x2223 => {
                let window = at(0x2220);
                self.rom_block[window] = field(value, 0, 3);
                self.rom_block_mode[window] = flag(value, 7);
            }
            0x2224 => self.scpu_bwram_block = field(value, 0, 5),
            0x2225 => {
                self.sa1_bwram_block = field(value, 0, 7);
                self.bwram_bitmap = flag(value, 7);
            }
            0x2226..=0x222A => self.write_protection[at(0x2226)] = value,
            0x2230 => self.dma.write_control(value),
            0x2231 => self.dma.write_conversion(value),
            0x2232..=0x2234 => self.dma.write_source_address(at(0x2232), value),
            0x2235..=0x2237 => match self.dma.write_destination_address(at(0x2235), value) {
                Some(Start::Copy) => self.run_copy(),
                Some(Start::ConversionOnRead) => self.arm_conversion(),
                None => {}
            },
            0x2238 | 0x2239 => self.dma.write_count(at(0x2238), value),
            0x223F => self.dma.write_bitmap_format(value),
            0x2240..=0x224F => self
                .dma
                .write_bitmap_register(at(0x2240), value, &mut self.iram),
            0x2250..=0x2254 => self.math.write(reg, value),
            0x2258 => self.varbit.write_control(value),
            0x2259..=0x225B => self.varbit.write_address(at(0x2259), value),
            _ => {}
        }
    }

    /// CCNT (`$2200`): the S-CPU's control of the SA-1. Bit 5 holds the
    /// SA-1's CPU in reset and bit 6 pauses it; bits 7, 4 and 0-3 send it an
    /// IRQ, an NMI and a message ([`Interrupts`]).
    ///
    /// The CPU starts again at CRV, in bank `$00`, on a write that finds it
    /// held in reset and has bit 7 clear, the condition bsnes-plus and
    /// Snes9x apply (ares applies a clear bit 5 instead). The write can land
    /// while the CPU is out of the chip for a burst, so the start is applied
    /// by the next burst ([`Self::pending_reset`]).
    fn write_reset_control(&mut self, value: u8) {
        if self.held_in_reset && !flag(value, 7) {
            self.pending_reset = Some(self.reset_vector);
        }
        self.paused = flag(value, 6);
        self.held_in_reset = flag(value, 5);
        self.interrupts.write_scpu_control(value);
    }

    /// A DMA copy, carried out at once through the chip's own views of its
    /// memories rather than the SA-1's bus: it can start from inside a burst
    /// (the SA-1 writing DDA), when the bus is already borrowed. A source
    /// that names no memory, or an address outside its memory, reads the
    /// open bus. The DMA interrupt fires at the end.
    fn run_copy(&mut self) {
        let copy = self.dma.take_copy();
        if !copy.within_one_memory() {
            for step in 0..u32::from(copy.bytes) {
                let value = self.copy_read(copy.source, (copy.from + step) & 0xFF_FFFF);
                self.copy_write(copy.to_bwram, (copy.to + step) & 0xFF_FFFF, value);
            }
        }
        self.interrupts.copy_finished();
    }

    /// One byte a copy reads: ROM where it shows at `$8000-$FFFF` or in
    /// `$C0-$FF`, BW-RAM through the SA-1's window or in order, I-RAM at any
    /// address (its low 11 bits).
    fn copy_read(&self, source: Source, address: u32) -> u8 {
        match source {
            Source::Rom if bus::copy_reads_rom(address) => self.rom_via_mmc(address),
            Source::Bwram if bus::in_bwram_window(address) => {
                self.bwram_window_sa1_read(address & 0x1FFF)
            }
            Source::Bwram if bus::in_bwram_banks(address) => {
                self.bwram_linear_read(address - 0x40_0000)
            }
            Source::Iram => self.iram_read(address & 0x7FF),
            _ => self.cpu_mdr,
        }
    }

    /// One byte a copy writes: I-RAM at any address, BW-RAM through the
    /// SA-1's window or in order; elsewhere it goes nowhere.
    fn copy_write(&mut self, to_bwram: bool, address: u32, value: u8) {
        if !to_bwram {
            self.iram_write(address & 0x7FF, value);
        } else if bus::in_bwram_window(address) {
            self.bwram_window_sa1_write(address & 0x1FFF, value);
        } else if bus::in_bwram_banks(address) {
            self.bwram_linear_write(address - 0x40_0000, value);
        }
    }

    /// The type-1 conversion is armed; it raises an IRQ to the S-CPU. The
    /// conversion itself happens as the S-CPU reads BW-RAM
    /// ([`Sa1Dma::convert_on_read`], from [`Coprocessor::read`]).
    fn arm_conversion(&mut self) {
        self.dma.arm_conversion();
        self.interrupts.conversion_armed();
    }

    /// A start the S-CPU asked for: the CPU begins at the address, in bank
    /// `$00`, and out of any WAI.
    fn apply_pending_reset(&mut self, cpu: &mut Wdc65816) {
        if let Some(vector) = self.pending_reset.take() {
            cpu.pc = vector;
            cpu.pbr = 0;
            cpu.waiting = false;
        }
    }

    /// Run one SA-1 instruction against a [`Sa1Bus`] and return what it
    /// cost in master cycles: two per tick of its accesses ([`Sa1Bus`]), two
    /// per cycle without an access. The CPU reports all its cycles, so those
    /// without an access are its count less the accesses (never below zero).
    /// At least two, so a burst always moves on: an instruction fetches at
    /// least its opcode, and WAI or STP report one cycle and no access.
    fn advance_one(&mut self, cpu: &mut Wdc65816) -> u64 {
        let (cpu_cycles, ticks, accesses) = {
            let mut bus = Sa1Bus::new(self);
            let cycles = cpu.step(&mut bus) as u64;
            (cycles, bus.ticks, bus.accesses)
        };
        let internal = cpu_cycles.saturating_sub(accesses);
        (ticks * 2 + internal * 2).max(2)
    }

    /// One instruction with its interrupts, as a burst runs it: the CPU is
    /// given the SA-1's IRQ line and the NMI's rise, the bus answers a vector
    /// fetch from the registers for the instruction that takes one, a taken
    /// NMI is acknowledged, and the timer moves on by the cycles spent, which
    /// are returned.
    fn run_one(&mut self, cpu: &mut Wdc65816) -> u64 {
        cpu.irq_pending = self.interrupts.sa1_irq_line();
        if self.interrupts.nmi_rose() {
            cpu.nmi_pending = true;
        }
        let taking_nmi = cpu.nmi_pending;
        let taking_irq = cpu.irq_pending && (cpu.p & FLAG_I) == 0;
        self.vector_redirect = taking_nmi || taking_irq;

        let spent = self.advance_one(cpu);
        self.vector_redirect = false;

        if taking_nmi && !cpu.nmi_pending {
            self.interrupts.nmi_taken();
        }
        self.sa1_master += spent;
        if self.timer.advance(spent) {
            self.interrupts.timer_fired();
        }
        spent
    }

    /// The debugger's single step: one SA-1 instruction, exactly as a burst
    /// runs it. While the CPU is held in reset or paused there is nothing to
    /// step: the step reports no instruction and leaves the time as it is
    /// (only the S-CPU's time moves the SA-1's).
    fn step_one(&mut self) -> StepResult {
        let mut cpu = std::mem::take(&mut self.cpu);
        self.apply_pending_reset(&mut cpu);
        if self.held_in_reset || self.paused {
            self.cpu = cpu;
            return StepResult {
                cycles: 0,
                breakpoint_hit: false,
                instruction_executed: false,
            };
        }
        let spent = self.run_one(&mut cpu);
        self.cpu = cpu;
        StepResult {
            cycles: spent.min(u32::MAX as u64) as u32,
            breakpoint_hit: false,
            instruction_executed: true,
        }
    }

    /// Test only: apply a waiting start, run one instruction, and return its
    /// cost, so a test can weigh a BW-RAM instruction against an I-RAM one.
    #[cfg(test)]
    fn test_single_step_cost(&mut self) -> u64 {
        let mut cpu = std::mem::take(&mut self.cpu);
        self.apply_pending_reset(&mut cpu);
        let spent = self.advance_one(&mut cpu);
        self.cpu = cpu;
        spent
    }
}

impl Coprocessor for Sa1Chip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, master_cycle: u64) -> u8 {
        // Bring the SA-1 up to the moment of this access first.
        self.catch_up(master_cycle);
        self.accesses += 1;
        if offset >= SA1_ROM_BASE {
            // The S-CPU's own NMI and IRQ vectors read from SNV and SIV when
            // SCNT says so.
            let snes_addr = offset - SA1_ROM_BASE;
            match self.interrupts.scpu_vector_byte(snes_addr) {
                Some(byte) => byte,
                None => self.rom_via_mmc(snes_addr),
            }
        } else if offset >= SA1_BWRAM_LINEAR_BASE {
            // BW-RAM in order. While the type-1 conversion is armed, the
            // S-CPU's reads of BW-RAM are answered from converted characters.
            let at = offset - SA1_BWRAM_LINEAR_BASE;
            if self.dma.conversion_armed() {
                self.dma.convert_on_read(&self.bwram, &mut self.iram, at)
            } else {
                self.bwram_linear_read(at)
            }
        } else if offset >= SA1_BWRAM_BASE {
            // The S-CPU's window, the same way; its offset into BW-RAM counts
            // from the BMAPS block.
            let rel = offset - SA1_BWRAM_BASE;
            if self.dma.conversion_armed() {
                let at = u32::from(self.scpu_bwram_block) * 0x2000 + (rel & 0x1FFF);
                self.dma.convert_on_read(&self.bwram, &mut self.iram, at)
            } else {
                self.bwram_window_read(rel)
            }
        } else if offset >= SA1_IRAM_BASE {
            self.iram_read(offset - SA1_IRAM_BASE)
        } else {
            let reg = 0x2200 + (offset.wrapping_sub(SA1_MMIO_BASE) & 0x1FF) as u16;
            self.mmio_read(reg)
        }
    }

    fn write(&mut self, offset: u32, value: u8, master_cycle: u64) {
        self.catch_up(master_cycle);
        self.accesses += 1;
        if offset >= SA1_ROM_BASE {
            // ROM takes no write.
        } else if offset >= SA1_BWRAM_LINEAR_BASE {
            self.bwram_linear_write(offset - SA1_BWRAM_LINEAR_BASE, value);
        } else if offset >= SA1_BWRAM_BASE {
            self.bwram_window_write(offset - SA1_BWRAM_BASE, value);
        } else if offset >= SA1_IRAM_BASE {
            self.iram_write(offset - SA1_IRAM_BASE, value);
        } else {
            let reg = 0x2200 + (offset.wrapping_sub(SA1_MMIO_BASE) & 0x1FF) as u16;
            self.mmio_write(reg, value);
        }
    }

    /// Run the SA-1 up to `master_cycle`, an instruction at a time. Called
    /// at each S-CPU access to the chip and once per S-CPU instruction
    /// (`system.rs`, for cartridges with a CPU of their own). A cycle at or
    /// before the SA-1's own does nothing. A start the S-CPU asked for is
    /// applied first. While the CPU is held in reset or paused it runs no
    /// instruction, but the time passes and the timer counts it, a target
    /// reached meanwhile is waiting when the SA-1 starts.
    fn catch_up(&mut self, master_cycle: u64) {
        if master_cycle <= self.sa1_master {
            return;
        }
        let mut cpu = std::mem::take(&mut self.cpu);
        self.apply_pending_reset(&mut cpu);
        while self.sa1_master < master_cycle {
            if self.held_in_reset || self.paused {
                if self.timer.advance(master_cycle - self.sa1_master) {
                    self.interrupts.timer_fired();
                }
                self.sa1_master = master_cycle;
                break;
            }
            self.run_one(&mut cpu);
        }
        self.cpu = cpu;
    }

    /// The SA-1's IRQ line to the S-CPU, which `system.rs` joins to the
    /// S-CPU's own each instruction.
    fn has_irq(&self) -> bool {
        self.interrupts.scpu_line()
    }

    /// The SA-1's registers for the debugger's third CPU, through the same
    /// [`cpu_state_65816`] the S-CPU uses; the console sets the slot.
    fn secondary_cpu_state(&self) -> Option<CpuState> {
        Some(cpu_state_65816(&self.cpu))
    }

    /// One SA-1 instruction ([`Self::step_one`]).
    fn step_secondary_cpu(&mut self) -> Option<StepResult> {
        Some(self.step_one())
    }

    /// Decode `count` SA-1 instructions from `addr`, changing nothing: over a
    /// [`Sa1PeekBus`] (the SA-1's map, its registers read as zero), with the
    /// CPU's current M, X and E flags, which set the width of an immediate
    /// operand, as the S-CPU's decode in `system.rs` does.
    fn disassemble_secondary(&self, addr: u32, count: u32) -> Vec<Instruction> {
        let state = wdc65816::disasm::DisasmState {
            emulation: self.cpu.emulation,
            flag_m: self.cpu.p & 0x20 != 0,
            flag_x: self.cpu.p & 0x10 != 0,
        };
        let mut out = Vec::with_capacity(count as usize);
        let mut cursor = addr & 0x00FF_FFFF;
        let mut bus = Sa1PeekBus { chip: self };
        for _ in 0..count {
            let res = wdc65816::disasm::disassemble_one(&mut bus, cursor, &state);
            let len = res.bytes_consumed.max(1);
            out.push(Instruction {
                address: cursor,
                bytes: res.bytes,
                mnemonic: res.mnemonic,
                operands: res.operands,
                comment: res.comment,
            });
            // The fetch wraps within the bank and keeps the program bank.
            let bank = cursor & 0x00FF_0000;
            let next = (cursor as u16).wrapping_add(len as u16);
            cursor = bank | next as u32;
        }
        out
    }

    /// Everything as at power-on, but BW-RAM, which a battery may keep, and
    /// I-RAM, which keeps its contents through a reset as static RAM does.
    fn reset(&mut self, master_clock_hz: u64) {
        self.rom_block = [0, 1, 2, 3];
        self.rom_block_mode = [false; 4];
        self.scpu_bwram_block = 0;
        self.sa1_bwram_block = 0;
        self.bwram_bitmap = false;
        self.write_protection = [0; 5];
        self.math.reset();
        self.cpu = Wdc65816::new();
        self.sa1_master = 0;
        self.held_in_reset = true;
        self.paused = false;
        self.reset_vector = 0;
        self.pending_reset = None;
        self.cpu_mdr = 0;
        self.accesses = 0;
        self.timer = Sa1Timer::default();
        self.timer
            .set_lines_per_frame(region_lines(master_clock_hz));
        self.dma = Sa1Dma::default();
        self.varbit = Sa1VarBit::default();
        self.interrupts = Interrupts::default();
        self.vector_redirect = false;
    }

    /// The savestate: the Super MMC, the BW-RAM selections and the write
    /// protection, the arithmetic unit, the CPU with its time and its reset
    /// control, I-RAM, and the timer, interrupts, DMA and bit reader. BW-RAM
    /// is saved with the battery ([`Coprocessor::battery`]) and ROM never
    /// changes, so neither is in it.
    fn serialize(&self) -> Vec<u8> {
        let state = CoprocessorState::Sa1(Sa1State {
            mmc_bank: self.rom_block,
            mmc_mode: self.rom_block_mode,
            sbm: self.scpu_bwram_block,
            cbm: self.sa1_bwram_block,
            sw46: self.bwram_bitmap,
            protect: self.write_protection,
            math: self.math.snapshot(),
            iram: self.iram.to_vec(),
            cpu: Cpu65816State::capture(&self.cpu),
            sa1_master: self.sa1_master,
            crv: self.reset_vector,
            sa1_resb: self.held_in_reset,
            sa1_rdyb: self.paused,
            pending_reset: self.pending_reset,
            timer: self.timer.snapshot(),
            irq: self.interrupts.snapshot(),
            dma: self.dma.snapshot(),
            varbit: self.varbit.snapshot(),
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        // A savestate that is damaged, empty, or another chip's leaves the
        // chip as it is.
        let Ok(CoprocessorState::Sa1(state)) = bincode::deserialize::<CoprocessorState>(data)
        else {
            return;
        };
        self.rom_block = state.mmc_bank;
        self.rom_block_mode = state.mmc_mode;
        self.scpu_bwram_block = state.sbm;
        self.sa1_bwram_block = state.cbm;
        self.bwram_bitmap = state.sw46;
        self.write_protection = state.protect;
        self.math.restore_state(&state.math);
        let n = self.iram.len().min(state.iram.len());
        self.iram[..n].copy_from_slice(&state.iram[..n]);
        state.cpu.restore(&mut self.cpu);
        self.sa1_master = state.sa1_master;
        self.reset_vector = state.crv;
        self.held_in_reset = state.sa1_resb;
        self.paused = state.sa1_rdyb;
        self.pending_reset = state.pending_reset;
        self.timer.restore(&state.timer);
        self.interrupts.restore(&state.irq);
        self.vector_redirect = false;
        self.dma.restore(&state.dma);
        self.varbit.restore(&state.varbit);
    }

    fn debug_state(&self) -> ExpansionChipState {
        let reg = |address: u32, name: &str, value: u32, description: &str| ChipRegister {
            address,
            name: name.to_string(),
            value,
            description: description.to_string(),
        };
        let rom_window_row = |addr: u32, name: &str, window: usize| {
            reg(
                addr,
                name,
                u32::from(self.rom_block[window]) | (u32::from(self.rom_block_mode[window]) << 8),
                "Super MMC block select (bits 0-2) + projection mode (bit 8)",
            )
        };
        let [nmi_vector, irq_vector, scpu_nmi_vector, scpu_irq_vector] = self.interrupts.vectors();
        let registers = vec![
            rom_window_row(0x2220, "CXB", 0),
            rom_window_row(0x2221, "DXB", 1),
            rom_window_row(0x2222, "EXB", 2),
            rom_window_row(0x2223, "FXB", 3),
            reg(
                0x2224,
                "BMAPS",
                u32::from(self.scpu_bwram_block),
                "S-CPU BW-RAM 8 KiB window block",
            ),
            reg(
                0x2225,
                "BMAP",
                u32::from(self.sa1_bwram_block) | (u32::from(self.bwram_bitmap) << 7),
                "SA-1 BW-RAM window block (bits 0-6) + bitmap view (bit 7)",
            ),
            reg(
                0x2250,
                "MCNT",
                u32::from(self.math.control_byte()),
                "Arithmetic control: accumulate (bit 1), divide (bit 0)",
            ),
            reg(
                0x2251,
                "MA",
                u32::from(self.math.operand_a()),
                "Arithmetic MA: multiplicand / dividend",
            ),
            reg(
                0x2253,
                "MB",
                u32::from(self.math.operand_b()),
                "Arithmetic MB: multiplier / divisor",
            ),
            reg(
                0x2306,
                "MR",
                self.math.result() as u32,
                "Arithmetic result MR, low 32 of 40 bits",
            ),
            reg(
                0x230A,
                "MRH",
                u32::from(self.math.result().byte(4)),
                "Arithmetic result MR, high 8 bits (32-39)",
            ),
            reg(
                0x230B,
                "OF",
                u32::from(self.math.overflow()) << 7,
                "Accumulation overflow (bit 7)",
            ),
            // The bit reader's width, mode and cursor, read without touching
            // VDPL/VDPH (a read of VDPH can move the cursor).
            reg(
                0x2258,
                "VBD",
                u32::from(self.varbit.width()) | (u32::from(self.varbit.advances_on_read()) << 7),
                "Bit reader width (bits 0-3, 1-16) + move on read (bit 7)",
            ),
            reg(
                0x2259,
                "VDA",
                self.varbit.address() | (u32::from(self.varbit.bit()) << 24),
                "Bit reader cursor: byte address (bits 0-23) + bit (24-26)",
            ),
            // The timer. HCR/VCR show the counters as they are, not the copy
            // the last read of `$2302` made, which the SA-1's program may
            // still be reading.
            reg(
                0x2210,
                "TMC",
                u32::from(self.timer.control_byte()),
                "H/V timer control: linear (bit 7), V match (bit 1), H match (bit 0)",
            ),
            reg(
                0x2212,
                "HCNT",
                u32::from(self.timer.h_target()),
                "H-count compare target (dots)",
            ),
            reg(
                0x2214,
                "VCNT",
                u32::from(self.timer.v_target()),
                "V-count compare target",
            ),
            reg(
                0x2302,
                "HCR",
                u32::from(self.timer.h_counter() / 4),
                "Live H counter (dots), side-effect-free (no latch)",
            ),
            reg(
                0x2304,
                "VCR",
                u32::from(self.timer.v_counter()),
                "Live V counter, side-effect-free (no latch)",
            ),
            // Interrupts: CFR and SFR as the CPUs read them.
            reg(
                0x2301,
                "CFR",
                u32::from(self.interrupts.sa1_flags()),
                "SA-1 flags: IRQ (7) timer (6) DMA (5) NMI (4) + message (0-3)",
            ),
            reg(
                0x2300,
                "SFR",
                u32::from(self.interrupts.scpu_flags()),
                "S-CPU flags: IRQ (7) IRQ vector (6) conversion (5) NMI vector (4) + message (0-3)",
            ),
            reg(
                0x220A,
                "CIE",
                u32::from(self.interrupts.sa1_enable_byte()),
                "SA-1 interrupt enable: IRQ (7) timer (6) DMA (5) NMI (4)",
            ),
            reg(
                0x2201,
                "SIE",
                u32::from(self.interrupts.scpu_enable_byte()),
                "S-CPU interrupt enable: IRQ (7) conversion (5)",
            ),
            reg(0x2205, "CNV", u32::from(nmi_vector), "SA-1 NMI vector"),
            reg(0x2207, "CIV", u32::from(irq_vector), "SA-1 IRQ vector"),
            reg(
                0x220C,
                "SNV",
                u32::from(scpu_nmi_vector),
                "S-CPU NMI vector override",
            ),
            reg(
                0x220E,
                "SIV",
                u32::from(scpu_irq_vector),
                "S-CPU IRQ vector override",
            ),
            reg(
                0x2230,
                "DCNT",
                u32::from(self.dma.control_byte()),
                "DMA control: enable (7) priority (6) conversion (5) on read (4) to BW-RAM (2) source (0-1)",
            ),
            reg(0x2232, "SDA", self.dma.source_address(), "DMA source address (24-bit)"),
            reg(
                0x2235,
                "DDA",
                self.dma.destination_address(),
                "DMA destination address (24-bit)",
            ),
            reg(0x2238, "DTC", u32::from(self.dma.count()), "DMA byte count"),
            // The SA-1's CPU, read without effect, for the CLI's dump.
            // Address 0 marks a row that is not a register.
            reg(
                0,
                "SA1_PC",
                (u32::from(self.cpu.pbr) << 16) | u32::from(self.cpu.pc),
                "SA-1 CPU program counter (PBR:PC)",
            ),
            reg(
                0,
                "SA1_A",
                u32::from(self.cpu.c),
                "SA-1 CPU accumulator (C)",
            ),
            reg(0, "SA1_X", u32::from(self.cpu.x), "SA-1 CPU index X"),
            reg(0, "SA1_Y", u32::from(self.cpu.y), "SA-1 CPU index Y"),
            reg(0, "SA1_S", u32::from(self.cpu.sp), "SA-1 CPU stack pointer"),
            reg(0, "SA1_D", u32::from(self.cpu.d), "SA-1 CPU direct page"),
            reg(0, "SA1_DBR", u32::from(self.cpu.dbr), "SA-1 CPU data bank"),
            reg(0, "SA1_P", u32::from(self.cpu.p), "SA-1 CPU status (P)"),
            reg(
                0,
                "SA1_E",
                u32::from(self.cpu.emulation),
                "SA-1 CPU emulation-mode flag",
            ),
            reg(
                0x2200,
                "SLEEP",
                (u32::from(self.paused) << 6) | (u32::from(self.held_in_reset) << 5),
                "SA-1 CPU asleep: paused (bit 6) / held in reset (bit 5)",
            ),
            reg(0x2203, "CRV", u32::from(self.reset_vector), "SA-1 CPU reset vector"),
            reg(0, "BWRAM", self.bwram.len() as u32, "BW-RAM size (bytes)"),
            reg(
                0,
                "BATTERY",
                u32::from(self.has_battery),
                "BW-RAM battery-backed (1 = yes)",
            ),
            reg(
                0,
                "ACCESSES",
                self.accesses as u32,
                "CPU accesses since reset (truncated)",
            ),
        ];
        ExpansionChipState {
            chip_type: ExpansionChipType::SA1,
            registers,
            internal_ram: Some(self.iram.to_vec()),
            program_counter: Some((u32::from(self.cpu.pbr) << 16) | u32::from(self.cpu.pc)),
            cycle_count: self.cpu.cycles_executed,
        }
    }

    fn label(&self) -> &str {
        "SA-1"
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }

    fn battery(&self) -> Option<&[u8]> {
        // A cartridge without a battery loses its BW-RAM at power-off, so it
        // has nothing to save.
        if self.has_battery && !self.bwram.is_empty() {
            Some(&self.bwram)
        } else {
            None
        }
    }

    fn restore_battery(&mut self, data: &[u8]) {
        let n = data.len().min(self.bwram.len());
        self.bwram[..n].copy_from_slice(&data[..n]);
        self.bwram_dirty = false;
    }

    fn battery_dirty(&self) -> bool {
        self.bwram_dirty
    }

    fn clear_battery_dirty(&mut self) {
        self.bwram_dirty = false;
    }
}

/// The SA-1's view of memory for the disassembler, which changes nothing.
///
/// The same map as [`Sa1Bus`] (I-RAM, the BW-RAM window, the banks in order
/// and the bitmap, ROM through the Super MMC), with two differences. The
/// registers (`$2200-$23FF`) read as zero, because a real read can copy the
/// timer's counters or move the bit reader on, and code never runs from
/// there. And a read costs no time and leaves the data bus as it was.
/// Writes go nowhere.
struct Sa1PeekBus<'a> {
    chip: &'a Sa1Chip,
}

impl Addressable for Sa1PeekBus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        let addr = addr & 0xFF_FFFF;
        let bank = (addr >> 16) as u8;
        let off = addr as u16;
        match bank {
            0x00..=0x3F | 0x80..=0xBF => match off {
                0x0000..=0x07FF | 0x3000..=0x37FF => self.chip.iram_read(u32::from(off)),
                0x2200..=0x23FF => 0,
                0x6000..=0x7FFF => self.chip.bwram_window_sa1_read(u32::from(off - 0x6000)),
                0x8000..=0xFFFF => self.chip.rom_via_mmc(addr),
                _ => self.chip.cpu_mdr,
            },
            0x40..=0x4F => self.chip.bwram_linear_read(addr - 0x40_0000),
            0x60..=0x6F => bitmap::bitmap_read(
                &self.chip.bwram,
                addr - 0x60_0000,
                self.chip.dma.bitmap_2bpp(),
            ),
            0xC0..=0xFF => self.chip.rom_via_mmc(addr),
            _ => self.chip.cpu_mdr,
        }
    }

    fn write(&mut self, _addr: u32, _value: u8) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_HZ: u64 = 21_477_270;

    /// A ROM whose every byte encodes its own offset (low byte), so a read-back
    /// uniquely identifies which ROM offset the Super MMC resolved.
    fn ramp_rom(len: usize) -> Arc<[u8]> {
        let bytes: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
        Arc::from(bytes.into_boxed_slice())
    }

    /// Read a ROM byte through the public [`Coprocessor::read`] path, as the
    /// bus would after applying the ROM slot's `SA1_ROM_BASE + addr24` base.
    fn read_rom_addr(chip: &mut Sa1Chip, addr24: u32) -> u8 {
        chip.read(SA1_ROM_BASE + addr24, 0)
    }

    fn write_mmio(chip: &mut Sa1Chip, reg: u16, value: u8) {
        chip.write(SA1_MMIO_BASE + u32::from(reg - 0x2200), value, 0);
    }

    fn read_mmio(chip: &mut Sa1Chip, reg: u16) -> u8 {
        chip.read(SA1_MMIO_BASE + u32::from(reg - 0x2200), 0)
    }

    #[test]
    fn label_type_and_reset_defaults() {
        let chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        assert_eq!(chip.label(), "SA-1");
        assert_eq!(chip.info().chip_type, ExpansionChipType::SA1);
        // Reset default banking is 0/1/2/3, LoROM-fixed projection.
        assert_eq!(chip.rom_block, [0, 1, 2, 3]);
        assert_eq!(chip.rom_block_mode, [false; 4]);
    }

    #[test]
    fn default_mmc_maps_first_four_mib_linearly() {
        let rom = ramp_rom(0x40_0000); // 4 MiB
        let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
        // $00:8000 -> block C LoROM-fixed base 0 -> ROM offset 0.
        assert_eq!(read_rom_addr(&mut chip, 0x00_8000), rom[0]);
        // $01:8000 -> 1 * 0x8000 (32 KiB/bank).
        assert_eq!(read_rom_addr(&mut chip, 0x01_8000), rom[0x8000]);
        // $20:8000 -> block D fixed base 0x100000.
        assert_eq!(read_rom_addr(&mut chip, 0x20_8000), rom[0x10_0000]);
        // $80:8000 -> block E fixed base 0x200000.
        assert_eq!(read_rom_addr(&mut chip, 0x80_8000), rom[0x20_0000]);
        // $A0:8000 -> block F fixed base 0x300000.
        assert_eq!(read_rom_addr(&mut chip, 0xA0_8000), rom[0x30_0000]);
    }

    #[test]
    fn cx_fx_window_is_linear_64k_per_bank() {
        let rom = ramp_rom(0x40_0000);
        let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
        // $C0:0000 -> block C selected (bank 0) -> ROM offset 0.
        assert_eq!(read_rom_addr(&mut chip, 0xC0_0000), rom[0]);
        // $C1:0000 -> +0x10000 (64 KiB/bank linear).
        assert_eq!(read_rom_addr(&mut chip, 0xC1_0000), rom[0x1_0000]);
        // $D0:0000 -> block D selected (bank 1) -> 0x100000.
        assert_eq!(read_rom_addr(&mut chip, 0xD0_0000), rom[0x10_0000]);
        // $F0:0000 -> block F selected (bank 3) -> 0x300000.
        assert_eq!(read_rom_addr(&mut chip, 0xF0_0000), rom[0x30_0000]);
    }

    #[test]
    fn mmc_bmode_zero_pins_8000_window_to_fixed_projection() {
        let rom = ramp_rom(0x80_0000); // 8 MiB, blocks 0..7
        let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
        // Select block 5 for C but keep bmode 0: the $8000 window stays on the
        // fixed C projection (base 0), while the $Cx window follows the select.
        write_mmio(&mut chip, 0x2220, 5); // cb = 5, cbmode = 0
        assert_eq!(
            read_rom_addr(&mut chip, 0x00_8000),
            rom[0],
            "bmode0 -> fixed"
        );
        assert_eq!(
            read_rom_addr(&mut chip, 0xC0_0000),
            rom[5 << 20],
            "$Cx always follows the select"
        );
    }

    #[test]
    fn mmc_bmode_one_puts_selected_block_in_8000_window() {
        let rom = ramp_rom(0x80_0000);
        let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
        // cb = 6, cbmode = 1 (bit7): the $8000 window now projects block 6.
        write_mmio(&mut chip, 0x2220, 0x80 | 6);
        assert_eq!(read_rom_addr(&mut chip, 0x00_8000), rom[6 << 20]);
        assert_eq!(read_rom_addr(&mut chip, 0x01_8000), rom[(6 << 20) + 0x8000]);
    }

    #[test]
    fn iram_is_scpu_visible_only_at_3000_37ff() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // Write + read back through the I-RAM window (the bus hands the chip
        // SA1_IRAM_BASE + (addr - 0x3000)).
        chip.write(SA1_IRAM_BASE + 0x10, 0xAB, 0);
        assert_eq!(chip.read(SA1_IRAM_BASE + 0x10, 0), 0xAB);
        // The 2 KiB wraps: offset 0x800 aliases offset 0.
        chip.write(SA1_IRAM_BASE, 0xCD, 0);
        assert_eq!(chip.read(SA1_IRAM_BASE + 0x800, 0), 0xCD);
    }

    #[test]
    fn bwram_window_follows_the_2224_block_select() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ); // 32 KiB
                                                                                 // Default sbm=0: window byte 0 is BW-RAM byte 0.
        chip.write(SA1_BWRAM_BASE, 0x11, 0);
        assert_eq!(chip.read(SA1_BWRAM_BASE, 0), 0x11);
        // sbm=1: the 8 KiB window now points at BW-RAM offset 0x2000.
        write_mmio(&mut chip, 0x2224, 1);
        chip.write(SA1_BWRAM_BASE, 0x22, 0);
        assert_eq!(chip.read(SA1_BWRAM_BASE, 0), 0x22);
        // The linear view confirms the two writes landed at 0 and 0x2000.
        assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE, 0), 0x11);
        assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE + 0x2000, 0), 0x22);
    }

    #[test]
    fn bmap_stores_cbm_and_sw46_bit() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        // $2225 splits into cbm (bits 0-6) and the sw46 bitmap-mode select
        // (bit 7). A value with bit 7 clear stores cbm only.
        write_mmio(&mut chip, 0x2225, 0x2A);
        assert_eq!(chip.sa1_bwram_block, 0x2A);
        assert!(!chip.bwram_bitmap, "bit 7 clear -> normal window mode");
        // Bit 7 set flips the bitmap-mode select and keeps only bits 0-6 in cbm.
        write_mmio(&mut chip, 0x2225, 0x80 | 0x35);
        assert_eq!(chip.sa1_bwram_block, 0x35, "cbm masked to 7 bits");
        assert!(chip.bwram_bitmap, "bit 7 set -> sw46 (bitmap mode)");
        // The debug row folds sw46 back into bit 7 of the BMAP value.
        let bmap = chip
            .debug_state()
            .registers
            .into_iter()
            .find(|r| r.name == "BMAP")
            .unwrap();
        assert_eq!(bmap.value, 0x35 | (1 << 7));
    }

    #[test]
    fn bwram_linear_view_spans_the_whole_buffer() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x1_0000, true, NTSC_HZ); // 64 KiB
        chip.write(SA1_BWRAM_LINEAR_BASE + 0xABCD, 0x5A, 0);
        assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE + 0xABCD, 0), 0x5A);
    }

    #[test]
    fn no_bwram_reads_zero_and_drops_writes() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        chip.write(SA1_BWRAM_BASE, 0xFF, 0);
        chip.write(SA1_BWRAM_LINEAR_BASE, 0xFF, 0);
        assert_eq!(chip.read(SA1_BWRAM_BASE, 0), 0);
        assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE, 0), 0);
    }

    #[test]
    fn mmio_reads_are_stubbed_zero_and_out_of_range_writes_ignored() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // SFR ($2300) reads back 0 at reset: no flag is set.
        assert_eq!(chip.read(SA1_MMIO_BASE + (0x2300 - 0x2200), 0), 0);
        // A write into the $2300-$23FF read range is a no-op (no panic, no
        // state change): the MMC still reads the default mapping afterwards.
        chip.write(SA1_MMIO_BASE + (0x2301 - 0x2200), 0xFF, 0);
        let rom = ramp_rom(0x40_0000);
        assert_eq!(read_rom_addr(&mut chip, 0x00_8000), rom[0]);
    }

    #[test]
    fn arithmetic_unit_runs_through_the_mmio_path() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // Multiply (MCNT=0): 100 x 3 = 300 = 0x0000_012C. The $2254 high byte
        // triggers; the 40-bit result reads little-endian at $2306+.
        write_mmio(&mut chip, 0x2250, 0x00);
        write_mmio(&mut chip, 0x2251, 100);
        write_mmio(&mut chip, 0x2252, 0);
        write_mmio(&mut chip, 0x2253, 3);
        write_mmio(&mut chip, 0x2254, 0);
        assert_eq!(read_mmio(&mut chip, 0x2306), 0x2C);
        assert_eq!(read_mmio(&mut chip, 0x2307), 0x01);
        assert_eq!(read_mmio(&mut chip, 0x2308), 0x00);

        // Divide (MCNT=1): 100 / 7 = 14 rem 2 -> quotient at $2306, remainder at
        // the high word ($2308).
        write_mmio(&mut chip, 0x2250, 0x01);
        write_mmio(&mut chip, 0x2251, 100);
        write_mmio(&mut chip, 0x2252, 0);
        write_mmio(&mut chip, 0x2253, 7);
        write_mmio(&mut chip, 0x2254, 0);
        assert_eq!(read_mmio(&mut chip, 0x2306), 14, "quotient");
        assert_eq!(read_mmio(&mut chip, 0x2308), 2, "remainder (bits 16-23)");
    }

    #[test]
    fn savestate_round_trips_the_math_unit() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // MAC 1 x -1 into a fresh accumulator -> mr = 2^40 - 1, overflow set.
        write_mmio(&mut chip, 0x2250, 0x02); // acm=1 (resets mr)
        write_mmio(&mut chip, 0x2251, 1);
        write_mmio(&mut chip, 0x2252, 0);
        write_mmio(&mut chip, 0x2253, 0xFF);
        write_mmio(&mut chip, 0x2254, 0xFF); // trigger
        let saved = chip.serialize();

        let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        restored.restore(&saved, NTSC_HZ);
        assert_eq!(read_mmio(&mut restored, 0x2306), 0xFF);
        assert_eq!(
            read_mmio(&mut restored, 0x230A),
            0xFF,
            "top byte of the 40-bit mr"
        );
        assert_eq!(
            read_mmio(&mut restored, 0x230B),
            0x80,
            "MAC overflow survives"
        );
        assert_eq!(restored.serialize(), saved, "resave is byte-identical");
    }

    #[test]
    fn reset_clears_the_math_unit() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        write_mmio(&mut chip, 0x2250, 0x02);
        write_mmio(&mut chip, 0x2251, 1);
        write_mmio(&mut chip, 0x2253, 0xFF);
        write_mmio(&mut chip, 0x2254, 0xFF); // MAC -> mr + overflow set
        assert_ne!(read_mmio(&mut chip, 0x2306), 0);
        chip.reset(NTSC_HZ);
        assert_eq!(read_mmio(&mut chip, 0x2306), 0, "reset clears MR");
        assert_eq!(
            read_mmio(&mut chip, 0x230B),
            0,
            "reset clears the overflow flag"
        );
    }

    #[test]
    fn battery_gated_on_header_flag() {
        // $35 cart: BW-RAM persists.
        let with = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        assert_eq!(with.battery().map(<[u8]>::len), Some(0x8000));
        // RAM without a battery: BW-RAM works but does not persist.
        let without = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, false, NTSC_HZ);
        assert!(without.battery().is_none());
        // No RAM at all: no battery.
        let none = Sa1Chip::new(ramp_rom(0x40_0000), 0, true, NTSC_HZ);
        assert!(none.battery().is_none());
    }

    #[test]
    fn battery_round_trips_and_tracks_dirty() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        assert!(!chip.battery_dirty());
        chip.write(SA1_BWRAM_LINEAR_BASE + 0x100, 0x99, 0);
        assert!(
            chip.battery_dirty(),
            "a real BW-RAM write dirties the battery"
        );
        let mut save = vec![0u8; 0x8000];
        save[0x100] = 0x99;
        save[0x7FFF] = 0x42;
        chip.restore_battery(&save);
        assert!(!chip.battery_dirty(), "restore clears the dirty flag");
        assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE + 0x7FFF, 0), 0x42);
    }

    #[test]
    fn reset_restores_default_banking_but_keeps_bwram() {
        let mut chip = Sa1Chip::new(ramp_rom(0x80_0000), 0x8000, true, NTSC_HZ);
        write_mmio(&mut chip, 0x2220, 0x80 | 7); // remap block C
        write_mmio(&mut chip, 0x2224, 3); // move the BW-RAM window
        write_mmio(&mut chip, 0x2225, 0x80 | 5); // set cbm + sw46
        chip.write(SA1_BWRAM_LINEAR_BASE + 0x10, 0x7E, 0);
        chip.reset(NTSC_HZ);
        assert_eq!(
            chip.rom_block,
            [0, 1, 2, 3],
            "reset restores default banking"
        );
        assert_eq!(chip.rom_block_mode, [false; 4]);
        assert_eq!(chip.scpu_bwram_block, 0);
        assert_eq!(chip.sa1_bwram_block, 0);
        assert!(!chip.bwram_bitmap, "reset clears the sw46 bitmap select");
        // BW-RAM survives the reset (battery).
        assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE + 0x10, 0), 0x7E);
    }

    #[test]
    fn accesses_count_all_windows_and_reset_clears() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        assert_eq!(chip.chip_accesses(), 0);
        read_rom_addr(&mut chip, 0x00_8000); // ROM read
        chip.read(SA1_IRAM_BASE, 0); // I-RAM read
        chip.read(SA1_BWRAM_BASE, 0); // BW-RAM read
        write_mmio(&mut chip, 0x2220, 1); // MMIO write
        assert_eq!(chip.chip_accesses(), 4);
        chip.reset(NTSC_HZ);
        assert_eq!(chip.chip_accesses(), 0);
    }

    #[test]
    fn savestate_round_trips_mapping_and_iram() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        // Drive every persisted field away from its reset default.
        write_mmio(&mut chip, 0x2220, 0x80 | 4);
        write_mmio(&mut chip, 0x2221, 5);
        write_mmio(&mut chip, 0x2224, 2); // sbm
        write_mmio(&mut chip, 0x2225, 0x80 | 3); // cbm=3 + sw46 (bitmap mode)
        write_mmio(&mut chip, 0x2229, 0x0F); // a protection latch
        chip.write(SA1_IRAM_BASE + 0x40, 0xC3, 0);

        let saved = chip.serialize();
        assert!(!saved.is_empty(), "SA-1 always persists its mapping state");

        let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        restored.restore(&saved, NTSC_HZ);
        assert_eq!(restored.rom_block, chip.rom_block);
        assert_eq!(restored.rom_block_mode, chip.rom_block_mode);
        assert_eq!(restored.scpu_bwram_block, 2);
        assert_eq!(restored.sa1_bwram_block, 3);
        assert!(
            restored.bwram_bitmap,
            "sw46 round-trips through the savestate"
        );
        assert_eq!(restored.write_protection[3], 0x0F); // $2229 -> index 3
        assert_eq!(restored.read(SA1_IRAM_BASE + 0x40, 0), 0xC3);
        // Re-serializing after the restore reproduces the same bytes.
        assert_eq!(restored.serialize(), saved, "resave must be byte-identical");
    }

    #[test]
    fn restore_ignores_garbage_and_foreign_variants() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        write_mmio(&mut chip, 0x2220, 0x80 | 5); // a non-default mapping
        let before = chip.serialize();
        chip.restore(&[1, 2, 3], NTSC_HZ); // garbage
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

    #[test]
    fn debug_state_surfaces_mmc_and_iram() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        write_mmio(&mut chip, 0x2221, 0x80 | 2); // db = 2, dbmode = 1
        let st = chip.debug_state();
        assert_eq!(st.chip_type, ExpansionChipType::SA1);
        assert_eq!(st.internal_ram.as_ref().map(Vec::len), Some(IRAM_LEN));
        let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
        for want in ["CXB", "DXB", "BMAPS", "BWRAM", "BATTERY", "ACCESSES"] {
            assert!(names.contains(&want), "missing debug row {want}");
        }
        let dxb = st.registers.iter().find(|r| r.name == "DXB").unwrap();
        assert_eq!(dxb.value, 2 | (1 << 8), "db=2 with projection mode set");
    }

    #[test]
    fn rom_mirrors_when_offset_exceeds_rom() {
        // A 2 MiB ROM: block E's fixed base (0x200000) wraps modulo the len.
        let rom = ramp_rom(0x20_0000);
        let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
        let got = read_rom_addr(&mut chip, 0x80_8000); // fixed base 0x200000
        assert_eq!(got, rom[0x20_0000 % rom.len()]);
    }

    // -- Second 65816 CPU: wake, catch-up/burst, savestate ----------------

    /// Load an SA-1 program into I-RAM starting at `offset`, via the S-CPU I-RAM
    /// window. `master_cycle = 0` keeps the SA-1 asleep during setup (a
    /// `catch_up(0)` while the anchor is 0 is a monotonic no-op).
    fn load_iram(chip: &mut Sa1Chip, offset: u32, program: &[u8]) {
        for (i, &byte) in program.iter().enumerate() {
            chip.write(SA1_IRAM_BASE + offset + i as u32, byte, 0);
        }
    }

    /// Read an I-RAM byte back through the S-CPU window without advancing the
    /// SA-1 (the `catch_up(0)` is a no-op once the anchor is past 0).
    fn peek_iram(chip: &mut Sa1Chip, offset: u32) -> u8 {
        chip.read(SA1_IRAM_BASE + offset, 0)
    }

    /// Point CRV at `vector` and release reset, CCNT with resb (bit 5), rdyb
    /// (bit 6) and the IRQ message (bit 7) all clear, which arms the deferred
    /// wake (`pending_reset = Some(crv)`).
    fn wake_at(chip: &mut Sa1Chip, vector: u16) {
        write_mmio(chip, 0x2203, (vector & 0xFF) as u8);
        write_mmio(chip, 0x2204, (vector >> 8) as u8);
        write_mmio(chip, 0x2200, 0x00);
    }

    #[test]
    fn sa1_wakes_and_executes() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // LDA #$42 ; STA $10 ; STP  (emulation mode, 8-bit A, D=0).
        load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
        wake_at(&mut chip, 0x0000);
        chip.catch_up(1000);
        assert_eq!(peek_iram(&mut chip, 0x10), 0x42, "SA-1 ran its program");
        assert!(chip.cpu.cycles_executed > 0, "the CPU actually stepped");
    }

    #[test]
    fn wake_loads_pc_from_crv() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // A decoy at offset 0 would write 0xBB; the real program at offset 8
        // writes 0xAA. Executing from CRV=8 proves PC was loaded from CRV.
        load_iram(&mut chip, 0, &[0xA9, 0xBB, 0x85, 0x20, 0xDB]);
        load_iram(&mut chip, 8, &[0xA9, 0xAA, 0x85, 0x20, 0xDB]);
        wake_at(&mut chip, 0x0008);
        chip.catch_up(1000);
        assert_eq!(
            peek_iram(&mut chip, 0x20),
            0xAA,
            "started at CRV, not $0000"
        );
    }

    #[test]
    fn catch_up_is_monotonic() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
        wake_at(&mut chip, 0x0000);
        chip.catch_up(1000);
        let anchor = chip.sa1_master;
        let cycles = chip.cpu.cycles_executed;
        assert_eq!(anchor, 1000, "asleep-or-STP burst lands exactly on target");
        // A target at or behind the anchor does nothing.
        chip.catch_up(500);
        chip.catch_up(1000);
        assert_eq!(chip.sa1_master, anchor, "backward/equal targets are no-ops");
        assert_eq!(
            chip.cpu.cycles_executed, cycles,
            "no extra instructions run"
        );
    }

    #[test]
    fn sleeping_consumes_time_without_executing() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // A program that WOULD write a sentinel, but the SA-1 is never woken.
        load_iram(&mut chip, 0, &[0xA9, 0x99, 0x85, 0x30, 0xDB]);
        chip.catch_up(5000);
        assert_eq!(chip.sa1_master, 5000, "an asleep SA-1 still consumes time");
        assert_eq!(
            chip.cpu.cycles_executed, 0,
            "but never executes an instruction"
        );
        assert_eq!(
            peek_iram(&mut chip, 0x30),
            0,
            "the sentinel was never written"
        );
    }

    #[test]
    fn handshake_via_iram() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // LDA $10 ; INC A ; STA $11 ; BRA * (spin). Reads a job, writes job+1.
        load_iram(&mut chip, 0, &[0xA5, 0x10, 0x1A, 0x85, 0x11, 0x80, 0xFE]);
        // S-CPU posts the job (SA-1 still asleep, mc=0), then wakes it.
        chip.write(SA1_IRAM_BASE + 0x10, 0x41, 0);
        wake_at(&mut chip, 0x0000);
        // The S-CPU reads the result: the per-access catch-up runs the SA-1
        // forward to this instant, so the response is already there.
        let result = chip.read(SA1_IRAM_BASE + 0x11, 100_000);
        assert_eq!(result, 0x42, "SA-1 answered job+1 via I-RAM");
    }

    #[test]
    fn bwram_access_costs_more_ticks() {
        // Same instruction (STA absolute-long), different target: the BW-RAM
        // store costs the extra wait-state tick that the I-RAM store does not.
        let mut iram_chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        load_iram(&mut iram_chip, 0, &[0x8F, 0x10, 0x00, 0x00]); // STA $000010 (I-RAM)
        wake_at(&mut iram_chip, 0x0000);
        let cost_iram = iram_chip.test_single_step_cost();

        let mut bwram_chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        load_iram(&mut bwram_chip, 0, &[0x8F, 0x00, 0x00, 0x40]); // STA $400000 (BW-RAM)
        wake_at(&mut bwram_chip, 0x0000);
        let cost_bwram = bwram_chip.test_single_step_cost();

        assert!(
            cost_bwram > cost_iram,
            "BW-RAM store ({cost_bwram}) must outcost the I-RAM store ({cost_iram})"
        );
    }

    #[test]
    fn savestate_round_trips_cpu_and_anchor() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        load_iram(
            &mut chip,
            0,
            &[0xA9, 0x33, 0x85, 0x12, 0xA9, 0x44, 0x85, 0x13, 0xDB],
        );
        wake_at(&mut chip, 0x0000);
        chip.catch_up(1000);
        let saved = chip.serialize();

        let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        restored.restore(&saved, NTSC_HZ);
        assert_eq!(restored.sa1_master, chip.sa1_master, "anchor round-trips");
        assert_eq!(restored.reset_vector, chip.reset_vector);
        assert_eq!(restored.held_in_reset, chip.held_in_reset);
        assert_eq!(restored.paused, chip.paused);
        assert_eq!(restored.pending_reset, chip.pending_reset);
        assert_eq!(restored.cpu.pc, chip.cpu.pc, "CPU PC round-trips");
        assert_eq!(restored.cpu.c, chip.cpu.c);
        assert_eq!(restored.cpu.sp, chip.cpu.sp);
        assert_eq!(restored.cpu.cycles_executed, chip.cpu.cycles_executed);
        // The written sentinels survived in I-RAM.
        assert_eq!(peek_iram(&mut restored, 0x12), 0x33);
        assert_eq!(peek_iram(&mut restored, 0x13), 0x44);
        assert_eq!(restored.serialize(), saved, "resave is byte-identical");
    }

    #[test]
    fn debug_state_surfaces_the_second_cpu() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
        wake_at(&mut chip, 0x0000);
        chip.catch_up(1000);
        let st = chip.debug_state();
        let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
        for want in ["SA1_PC", "SA1_A", "SA1_S", "SA1_P", "SA1_E", "SLEEP", "CRV"] {
            assert!(names.contains(&want), "missing SA-1 CPU debug row {want}");
        }
        assert!(st.program_counter.is_some(), "PC surfaces for the CLI dump");
        assert!(st.cycle_count > 0, "cycle_count reflects the SA-1 CPU");
        // Awake: SLEEP row reads 0 (resb+rdyb both clear).
        let sleep = st.registers.iter().find(|r| r.name == "SLEEP").unwrap();
        assert_eq!(sleep.value, 0, "SLEEP clear once the SA-1 is running");
    }

    // -- Debugger single-step / register snapshot / disassembly -----------

    #[test]
    fn step_secondary_cpu_advances_one_instruction_at_a_time() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // LDA #$42 ; STA $10 ; STP  (emulation mode, 8-bit A, D=0).
        load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
        wake_at(&mut chip, 0x0000);

        // First step applies the deferred CRV wake and runs LDA #$42.
        let r0 = chip
            .step_secondary_cpu()
            .expect("the SA-1 hosts a steppable CPU");
        assert!(r0.instruction_executed, "one instruction ran");
        assert!(r0.cycles > 0, "the step charged master clocks");
        assert_eq!(chip.cpu.c & 0xFF, 0x42, "A holds the loaded immediate");
        assert_eq!(peek_iram(&mut chip, 0x10), 0, "STA has not run yet");

        // Second step runs STA $10, the store lands only now.
        let r1 = chip.step_secondary_cpu().expect("still hosts a CPU");
        assert!(r1.instruction_executed);
        assert_eq!(
            peek_iram(&mut chip, 0x10),
            0x42,
            "STA ran on the second single-step"
        );
    }

    #[test]
    fn step_secondary_cpu_is_a_noop_while_held_in_reset() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // A program that WOULD write a sentinel, but the SA-1 is never woken,
        // so resb is held and a single-step must do nothing.
        load_iram(&mut chip, 0, &[0xA9, 0x99, 0x85, 0x30, 0xDB]);
        let r = chip
            .step_secondary_cpu()
            .expect("hosts a CPU even while asleep");
        assert!(!r.instruction_executed, "held in reset: nothing steps");
        assert_eq!(r.cycles, 0, "no clocks charged while parked");
        assert_eq!(
            peek_iram(&mut chip, 0x30),
            0,
            "the sentinel was never written"
        );
    }

    #[test]
    fn secondary_cpu_state_tracks_the_sa1_registers() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
        wake_at(&mut chip, 0x0000);

        // The chip renders its CPU through the shared 65816 helper, which
        // stamps cpu_id 0; the console rewrites it to the real slot.
        let before = chip.secondary_cpu_state().expect("hosts a CPU");
        assert_eq!(before.cpu_id, 0, "chip reports the raw 65816 snapshot");

        chip.step_secondary_cpu(); // LDA #$42
        let after = chip.secondary_cpu_state().unwrap();
        let a = after.registers.iter().find(|r| r.name == "A").unwrap();
        assert_eq!(a.value & 0xFF, 0x42, "A surfaces in the register snapshot");
    }

    #[test]
    fn disassemble_secondary_decodes_the_sa1_program() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // LDA #$42 ; STA $10 at I-RAM $0000 (the SA-1 sees I-RAM low at bank 0).
        load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10]);

        // Fresh CPU is in emulation mode -> 8-bit immediate, so LDA #$42 is two
        // bytes and STA $10 follows at $0002.
        let rows = chip.disassemble_secondary(0x0000, 2);
        assert_eq!(rows.len(), 2, "decoded two instructions");
        assert_eq!(rows[0].address, 0x0000);
        assert!(
            rows[0].mnemonic.to_uppercase().contains("LDA"),
            "first row is LDA, got {:?}",
            rows[0].mnemonic
        );
        assert_eq!(rows[1].address, 0x0002, "advanced past the 2-byte LDA #imm");
        assert!(
            rows[1].mnemonic.to_uppercase().contains("STA"),
            "second row is STA, got {:?}",
            rows[1].mnemonic
        );
    }

    #[test]
    fn disassemble_secondary_reads_rom_without_side_effects() {
        // A ROM whose bank-0 $8000 window holds RTS ($60). Decoding it must not
        // latch MMIO or advance any cursor, the peek bus reads ROM inertly.
        let mut rom = vec![0u8; 0x40_0000];
        rom[0] = 0x60; // block C fixed base 0 -> SA-1 $00:8000
        let chip = Sa1Chip::new(Arc::from(rom.into_boxed_slice()), 0, false, NTSC_HZ);
        let rows = chip.disassemble_secondary(0x00_8000, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].address, 0x00_8000);
        assert!(
            rows[0].mnemonic.to_uppercase().contains("RTS"),
            "decoded ROM opcode, got {:?}",
            rows[0].mnemonic
        );
    }

    // -- Interrupts, H/V timer, DMA ---------------------------------------

    /// The SA-1 takes a timer IRQ and vectors through CIV (`$2207/08`), not the
    /// `$FFEx` bus vector. The clear latch is disarmed first (CIC) so *only* a
    /// timer compare can raise the line, proving the timer->IRQ path.
    #[test]
    fn sa1_takes_timer_irq_via_civ() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // ISR at $40 writes a sentinel then halts; main clears I and spins.
        load_iram(&mut chip, 0x40, &[0xA9, 0x5A, 0x85, 0x30, 0xDB]);
        load_iram(&mut chip, 0, &[0x58, 0x80, 0xFE]); // CLI ; BRA *
        write_mmio(&mut chip, 0x2207, 0x40); // CIV -> $0040
        write_mmio(&mut chip, 0x2208, 0x00);
        write_mmio(&mut chip, 0x2212, 0x01); // HCNT = 1 dot (4 clocks)
        write_mmio(&mut chip, 0x2210, 0x01); // TMC: hen
        write_mmio(&mut chip, 0x220B, 0x40); // CIC: disarm timer_irqcl
        write_mmio(&mut chip, 0x220A, 0x40); // CIE: enable timer IRQ
        wake_at(&mut chip, 0x0000);
        chip.catch_up(5000);
        assert_eq!(
            peek_iram(&mut chip, 0x30),
            0x5A,
            "timer IRQ vectored through CIV"
        );
    }

    /// The same timer IRQ is masked while the SA-1's `I` flag is set: the flag
    /// still latches (CFR bit 6) but the ISR never runs.
    #[test]
    fn timer_irq_is_i_gated() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        load_iram(&mut chip, 0x40, &[0xA9, 0x5A, 0x85, 0x30, 0xDB]);
        load_iram(&mut chip, 0, &[0x80, 0xFE]); // BRA *, never clears I (reset P.I = 1)
        write_mmio(&mut chip, 0x2207, 0x40);
        write_mmio(&mut chip, 0x2208, 0x00);
        write_mmio(&mut chip, 0x2212, 0x01);
        write_mmio(&mut chip, 0x2210, 0x01);
        write_mmio(&mut chip, 0x220B, 0x40);
        write_mmio(&mut chip, 0x220A, 0x40);
        wake_at(&mut chip, 0x0000);
        chip.catch_up(5000);
        assert_ne!(
            read_mmio(&mut chip, 0x2301) & 0x40,
            0,
            "the timer fired (CFR timer flag latched)"
        );
        assert_eq!(
            peek_iram(&mut chip, 0x30),
            0,
            "but the I flag masked the IRQ"
        );
    }

    /// An S-CPU NMI message (CCNT `$2200` bit 4) is edge-triggered and vectors
    /// through CNV (`$2205/06`), ungated by `I`.
    #[test]
    fn sa1_takes_nmi_via_cnv() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        load_iram(&mut chip, 0x40, &[0xA9, 0xA5, 0x85, 0x31, 0xDB]);
        load_iram(&mut chip, 0, &[0x80, 0xFE]); // spin
        write_mmio(&mut chip, 0x2205, 0x40); // CNV -> $0040
        write_mmio(&mut chip, 0x2206, 0x00);
        wake_at(&mut chip, 0x0000);
        chip.catch_up(500);
        assert_eq!(peek_iram(&mut chip, 0x31), 0, "no NMI before the message");
        // S-CPU posts the NMI while the SA-1 is awake (resb stays 0).
        write_mmio(&mut chip, 0x2200, 0x10);
        chip.catch_up(2000);
        assert_eq!(peek_iram(&mut chip, 0x31), 0xA5, "NMI vectored through CNV");
    }

    /// The SA-1->S-CPU IRQ line ([`Coprocessor::has_irq`]) is raised by a SCNT
    /// (`$2209`) message once SIE (`$2201`) enables it, and lowered by SIC
    /// (`$2202`).
    #[test]
    fn scpu_irq_line_via_scnt_and_sic() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        assert!(!chip.has_irq(), "no IRQ at reset");
        write_mmio(&mut chip, 0x2201, 0x80); // SIE: cpu_irqen (flag still clear -> no line)
        assert!(!chip.has_irq());
        write_mmio(&mut chip, 0x2209, 0x80); // SCNT: cpu_irq message -> line asserts
        assert!(chip.has_irq(), "SCNT bit 7 raises the S-CPU IRQ line");
        write_mmio(&mut chip, 0x2202, 0x80); // SIC: cpu_irqcl -> line drops
        assert!(!chip.has_irq(), "SIC lowers the S-CPU IRQ line");
    }

    /// VSPROM: the S-CPU's own native NMI/IRQ vectors read SNV/SIV when SCNT
    /// (`$2209`) selects the override (bits 4/6), and ROM otherwise. Only the
    /// native pair (`$FFEA/EB`, `$FFEE/EF`) is overridden, the emulation pair
    /// (`$FFFx`) always reads ROM.
    #[test]
    fn vsprom_overrides_native_scpu_vectors() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        let rom_nmi = read_rom_addr(&mut chip, 0x00_FFEA);
        let rom_irq = read_rom_addr(&mut chip, 0x00_FFEE);
        let rom_emu_nmi = read_rom_addr(&mut chip, 0x00_FFFA);
        write_mmio(&mut chip, 0x220C, 0xBC); // SNV = 0x9ABC
        write_mmio(&mut chip, 0x220D, 0x9A);
        write_mmio(&mut chip, 0x220E, 0xF0); // SIV = 0xDEF0
        write_mmio(&mut chip, 0x220F, 0xDE);
        // Overrides disabled -> ROM.
        assert_eq!(read_rom_addr(&mut chip, 0x00_FFEA), rom_nmi);
        assert_eq!(read_rom_addr(&mut chip, 0x00_FFEE), rom_irq);
        // Enable nvsw (bit 4) + ivsw (bit 6), no message (bit 7 clear).
        write_mmio(&mut chip, 0x2209, 0x50);
        assert_eq!(read_rom_addr(&mut chip, 0x00_FFEA), 0xBC, "NMI lo -> SNV");
        assert_eq!(read_rom_addr(&mut chip, 0x00_FFEB), 0x9A, "NMI hi -> SNV");
        assert_eq!(read_rom_addr(&mut chip, 0x00_FFEE), 0xF0, "IRQ lo -> SIV");
        assert_eq!(read_rom_addr(&mut chip, 0x00_FFEF), 0xDE, "IRQ hi -> SIV");
        assert_eq!(
            read_rom_addr(&mut chip, 0x00_FFFA),
            rom_emu_nmi,
            "emulation NMI vector is not VSPROM-overridden"
        );
    }

    /// A normal DMA copies ROM -> I-RAM instantaneously on the `$2236` (I-RAM
    /// destination) trigger and latches the DMA-end flag (CFR bit 5).
    #[test]
    fn dma_normal_rom_to_iram() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        write_mmio(&mut chip, 0x2230, 0x80); // DCNT: dmaen, cden=0, dd=IRAM, sd=ROM
        write_mmio(&mut chip, 0x2232, 0x00); // SDA = $008000 (ROM window -> ramp 0,1,2,3)
        write_mmio(&mut chip, 0x2233, 0x80);
        write_mmio(&mut chip, 0x2234, 0x00);
        write_mmio(&mut chip, 0x2238, 0x04); // DTC = 4
        write_mmio(&mut chip, 0x2239, 0x00);
        write_mmio(&mut chip, 0x2235, 0x10); // DDA lo = I-RAM offset $10
        write_mmio(&mut chip, 0x2236, 0x00); // DDA mid -> fires the transfer
        for i in 0..4u32 {
            assert_eq!(peek_iram(&mut chip, 0x10 + i), i as u8, "copied byte {i}");
        }
        assert_ne!(read_mmio(&mut chip, 0x2301) & 0x20, 0, "dma_irqfl latched");
    }

    /// A copy within one memory (I-RAM -> I-RAM) moves nothing, yet still
    /// raises the DMA-end flag.
    #[test]
    fn dma_normal_same_device_is_skipped() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        load_iram(&mut chip, 0x00, &[0x11]); // source sentinel
        load_iram(&mut chip, 0x40, &[0x22]); // destination sentinel
        write_mmio(&mut chip, 0x2230, 0x82); // DCNT: dmaen, dd=IRAM(0), sd=IRAM(2)
        write_mmio(&mut chip, 0x2232, 0x00);
        write_mmio(&mut chip, 0x2233, 0x00);
        write_mmio(&mut chip, 0x2234, 0x00); // SDA = $000000
        write_mmio(&mut chip, 0x2238, 0x04);
        write_mmio(&mut chip, 0x2239, 0x00); // DTC = 4
        write_mmio(&mut chip, 0x2235, 0x40); // DDA lo = $40
        write_mmio(&mut chip, 0x2236, 0x00); // trigger
        assert_eq!(
            peek_iram(&mut chip, 0x40),
            0x22,
            "I-RAM -> I-RAM transfer skipped"
        );
        assert_ne!(
            read_mmio(&mut chip, 0x2301) & 0x20,
            0,
            "dma_irqfl still set"
        );
    }

    /// The full timer + interrupt-controller + DMA-register-file state
    /// round-trips through the savestate and re-serializes byte-identically,
    /// including mid-execution timer counters and a live S-CPU IRQ line.
    #[test]
    fn savestate_round_trips_timer_irq_dma() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        write_mmio(&mut chip, 0x2210, 0x01); // TMC: hen
        write_mmio(&mut chip, 0x2212, 0x20); // HCNT
        write_mmio(&mut chip, 0x2214, 0x05); // VCNT
        write_mmio(&mut chip, 0x2205, 0x34); // CNV = 0x1234
        write_mmio(&mut chip, 0x2206, 0x12);
        write_mmio(&mut chip, 0x2207, 0x78); // CIV = 0x5678
        write_mmio(&mut chip, 0x2208, 0x56);
        write_mmio(&mut chip, 0x220C, 0xBC); // SNV = 0x9ABC
        write_mmio(&mut chip, 0x220D, 0x9A);
        write_mmio(&mut chip, 0x220E, 0xF0); // SIV = 0xDEF0
        write_mmio(&mut chip, 0x220F, 0xDE);
        write_mmio(&mut chip, 0x2201, 0x80); // SIE: cpu_irqen
        write_mmio(&mut chip, 0x2209, 0x80); // SCNT message -> scpu_irq
        write_mmio(&mut chip, 0x220A, 0x40); // CIE: timer_irqen
        write_mmio(&mut chip, 0x2230, 0x00); // DCNT (dmaen=0: store, no trigger)
        write_mmio(&mut chip, 0x2232, 0x56); // SDA = 0x123456
        write_mmio(&mut chip, 0x2233, 0x34);
        write_mmio(&mut chip, 0x2234, 0x12);
        write_mmio(&mut chip, 0x2238, 0x42); // DTC lo
        for i in 0..16u16 {
            write_mmio(&mut chip, 0x2240 + i, (0xF0 | i) as u8); // BRF
        }
        chip.catch_up(3000); // asleep, but the H/V timer advances

        let saved = chip.serialize();
        let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        restored.restore(&saved, NTSC_HZ);
        assert_eq!(restored.serialize(), saved, "resave is byte-identical");
        assert!(
            restored.has_irq(),
            "the S-CPU IRQ line survives the savestate"
        );
        let rows = restored.debug_state().registers;
        let field = |name: &str| rows.iter().find(|r| r.name == name).unwrap().value;
        assert_eq!(field("CIV"), 0x5678);
        assert_eq!(field("CNV"), 0x1234);
        assert_eq!(field("SIV"), 0xDEF0);
        assert_eq!(field("SDA"), 0x12_3456);
    }

    /// The H/V timer keeps counting while the SA-1 CPU is asleep, so a target
    /// reached during sleep is waiting when it wakes.
    #[test]
    fn timer_advances_while_asleep() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        write_mmio(&mut chip, 0x2210, 0x01); // hen
        write_mmio(&mut chip, 0x2212, 0x10); // HCNT = 16 dots (64 clocks)
        write_mmio(&mut chip, 0x220B, 0x40); // disarm
        write_mmio(&mut chip, 0x220A, 0x40); // enable timer IRQ
                                             // Never woken: the asleep branch still advances the timer.
        chip.catch_up(5000);
        assert_ne!(
            read_mmio(&mut chip, 0x2301) & 0x40,
            0,
            "timer flag latched while asleep"
        );
    }

    /// Arming online CC-DMA type 1 (writing `$2236` with `cden && cdsel`) raises
    /// the CHDMA IRQ to the S-CPU, and the S-CPU's subsequent BW-RAM read
    /// converts the bitmap character into I-RAM. 2bpp: a single low bit in the
    /// source lands in plane 0, column 0 (output bit 7).
    #[test]
    fn cc1_arm_raises_irq_and_converts_on_read() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        // Seed BW-RAM byte 0 (the char source) with pixel-0 plane-0 set.
        chip.write(SA1_BWRAM_LINEAR_BASE, 0x01, 0);
        write_mmio(&mut chip, 0x2201, 0x20); // SIE: chdma_irqen
        assert!(!chip.has_irq(), "no IRQ before the arm");
        write_mmio(&mut chip, 0x2230, 0xB0); // DCNT: dmaen + cden + cdsel
        write_mmio(&mut chip, 0x2231, 0x02); // CDMA: dmacb = 2 (2bpp)
        write_mmio(&mut chip, 0x2232, 0x00); // dsa = 0
        write_mmio(&mut chip, 0x2233, 0x00);
        write_mmio(&mut chip, 0x2234, 0x00);
        write_mmio(&mut chip, 0x2235, 0x00); // dda lo = 0
        write_mmio(&mut chip, 0x2236, 0x00); // dda mid -> arms CC1 + raises CHDMA IRQ
        assert!(chip.dma.conversion_armed(), "the $2236 write armed CC1");
        assert!(chip.has_irq(), "CHDMA IRQ asserts the S-CPU IRQ line");

        let byte = chip.read(SA1_BWRAM_LINEAR_BASE, 0);
        assert_eq!(
            byte, 0x80,
            "boundary read returns the buffered plane-0 byte"
        );
        assert_eq!(peek_iram(&mut chip, 0), 0x80, "I-RAM plane 0, column 0");
        assert_eq!(peek_iram(&mut chip, 1), 0x00, "I-RAM plane 1 clear");
    }

    /// CC-DMA type 1 also converts through the S-CPU's 8 KiB BW-RAM window
    /// (`$6000-7FFF`), folding the window into BW-RAM via `sbm`. 4bpp: a full low
    /// nibble sets column 0 across all four planes (planar indices `{0,1,16,17}`).
    #[test]
    fn cc1_converts_through_windowed_view_4bpp() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        write_mmio(&mut chip, 0x2224, 0x01); // sbm = 1 -> window maps to BW-RAM $2000
        chip.write(SA1_BWRAM_LINEAR_BASE + 0x2000, 0x0F, 0); // colour 15 at the source
        write_mmio(&mut chip, 0x2230, 0xB0); // dmaen + cden + cdsel
        write_mmio(&mut chip, 0x2231, 0x01); // dmacb = 1 (4bpp)
        write_mmio(&mut chip, 0x2232, 0x00); // dsa = 0x2000
        write_mmio(&mut chip, 0x2233, 0x20);
        write_mmio(&mut chip, 0x2234, 0x00);
        write_mmio(&mut chip, 0x2235, 0x00); // dda = 0
        write_mmio(&mut chip, 0x2236, 0x00); // arm

        // Read the window at $6000 (sbm=1 -> cartram offset 0x2000 = dsa, a
        // char boundary), which buffers the converted tile into I-RAM.
        let byte = chip.read(SA1_BWRAM_BASE, 0);
        assert_eq!(byte, 0x80);
        for &p in &[0u32, 1, 16, 17] {
            assert_eq!(peek_iram(&mut chip, p), 0x80, "plane at planar index {p}");
        }
        assert_eq!(peek_iram(&mut chip, 32), 0x00, "8bpp-only plane untouched");
    }

    /// A `chdend` write (`$2231` bit 7) ends the online run: the armed flag
    /// drops and the S-CPU's BW-RAM reads return the raw byte again.
    #[test]
    fn cc1_chdend_disarms_and_reads_go_raw() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        chip.write(SA1_BWRAM_LINEAR_BASE, 0x01, 0);
        write_mmio(&mut chip, 0x2230, 0xB0);
        write_mmio(&mut chip, 0x2231, 0x02); // 2bpp
        write_mmio(&mut chip, 0x2236, 0x00); // arm
        assert_eq!(
            chip.read(SA1_BWRAM_LINEAR_BASE, 0),
            0x80,
            "armed -> converted"
        );

        write_mmio(&mut chip, 0x2231, 0x80); // chdend -> disarm
        assert!(!chip.dma.conversion_armed());
        assert_eq!(
            chip.read(SA1_BWRAM_LINEAR_BASE, 0),
            0x01,
            "disarmed -> raw BW-RAM byte"
        );
    }

    /// The CC1 armed flag, the BW-RAM bitmap format (`bbf`), and the SA-1 bitmap
    /// window select (`sw46`) all round-trip through the savestate.
    #[test]
    fn savestate_round_trips_cc1_and_bitmap() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        write_mmio(&mut chip, 0x223F, 0x80); // bbf = 2bpp
        write_mmio(&mut chip, 0x2225, 0x80 | 0x05); // sw46 + cbm = 5
        write_mmio(&mut chip, 0x2230, 0xB0); // dmaen + cden + cdsel
        write_mmio(&mut chip, 0x2231, 0x02); // dmacb = 2
        write_mmio(&mut chip, 0x2235, 0x00);
        write_mmio(&mut chip, 0x2236, 0x00); // arm CC1
        assert!(chip.dma.conversion_armed());

        let saved = chip.serialize();
        let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        restored.restore(&saved, NTSC_HZ);
        assert_eq!(restored.serialize(), saved, "resave is byte-identical");
        assert!(restored.dma.conversion_armed(), "cc1_armed restored");
        assert!(restored.dma.bitmap_2bpp(), "bbf restored");
        assert!(restored.bwram_bitmap, "sw46 restored");
    }

    /// Reset disarms a type-1 conversion that was armed.
    #[test]
    fn reset_clears_cc1_armed() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        write_mmio(&mut chip, 0x2230, 0xB0);
        write_mmio(&mut chip, 0x2231, 0x02);
        write_mmio(&mut chip, 0x2236, 0x00); // arm
        assert!(chip.dma.conversion_armed());
        chip.reset(NTSC_HZ);
        assert!(!chip.dma.conversion_armed(), "reset disarms CC1");
    }

    /// Point the reader at a ROM address (`va`) and set a 24-bit stream cursor.
    fn set_vda(chip: &mut Sa1Chip, va: u32) {
        write_mmio(chip, 0x2259, (va & 0xFF) as u8);
        write_mmio(chip, 0x225A, ((va >> 8) & 0xFF) as u8);
        write_mmio(chip, 0x225B, ((va >> 16) & 0xFF) as u8); // also zeroes vbit
    }

    /// The variable-length reader pulls its bytes through the Super MMC from ROM
    /// (`$C0-FF` linear), and VDPH in auto-increment mode steps the cursor to the
    /// next field. The ramp ROM yields byte == offset.
    #[test]
    fn varbit_extracts_rom_field_and_auto_increments() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // $C0:0000 -> block C bank 0 -> ROM offset 0, so va+n reads ROM[n].
        set_vda(&mut chip, 0xC0_0000);
        write_mmio(&mut chip, 0x2258, 0x88); // hl = 1 (auto), vb = 8
                                             // vbit = 0: VDPL = ROM[0], VDPH high byte = ROM[1]. VDPL never advances.
        assert_eq!(read_mmio(&mut chip, 0x230C), 0x00, "VDPL = ROM[0]");
        assert_eq!(
            read_mmio(&mut chip, 0x230C),
            0x00,
            "VDPL is side-effect-free"
        );
        // Reading VDPH advances the cursor by the width (8 bits -> one byte).
        assert_eq!(
            read_mmio(&mut chip, 0x230D),
            0x01,
            "VDPH high byte = ROM[1]"
        );
        assert_eq!(
            read_mmio(&mut chip, 0x230C),
            0x01,
            "cursor advanced to ROM[1]"
        );
        assert_eq!(read_mmio(&mut chip, 0x230D), 0x02, "and again to ROM[2]");
    }

    /// The bit reader's view (`Sa1Chip::vbr_read`) maps I-RAM (`$0000-07FF`)
    /// and the *linear* BW-RAM views (`$40-4F` and the tiled
    /// `$00-3F:6000-7FFF` window). A 4-bit fixed-mode shift extracts a field
    /// straddling two I-RAM bytes.
    #[test]
    fn varbit_reads_iram_and_bwram_via_vbrbus() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
        // I-RAM stream 0x21 0x43 0x65 -> little-endian window 0x654321.
        load_iram(&mut chip, 0, &[0x21, 0x43, 0x65]);
        set_vda(&mut chip, 0x00_0000); // $00:0000 -> I-RAM low mirror, vbit = 0
        write_mmio(&mut chip, 0x2258, 0x04); // fixed, vb = 4 -> advance parks vbit at 4
                                             // window >> 4 = 0x065432 -> VDPL = 0x32, VDPH = 0x54 (fixed VDPH is inert).
        assert_eq!(read_mmio(&mut chip, 0x230C), 0x32, "I-RAM cross-byte low");
        assert_eq!(read_mmio(&mut chip, 0x230D), 0x54, "I-RAM cross-byte high");

        // Linear BW-RAM at $40:0100.
        chip.write(SA1_BWRAM_LINEAR_BASE + 0x100, 0xBE, 0);
        chip.write(SA1_BWRAM_LINEAR_BASE + 0x101, 0xEF, 0);
        set_vda(&mut chip, 0x40_0100);
        write_mmio(&mut chip, 0x2258, 0x88); // hl = 1, vb = 8
        assert_eq!(read_mmio(&mut chip, 0x230C), 0xBE, "linear BW-RAM $40:0100");
        assert_eq!(read_mmio(&mut chip, 0x230D), 0xEF, "linear BW-RAM $40:0101");

        // The tiled `$00:6000` window aliases BW-RAM offset 0 (8 KiB/bank).
        chip.write(SA1_BWRAM_LINEAR_BASE, 0x77, 0);
        set_vda(&mut chip, 0x00_6000);
        write_mmio(&mut chip, 0x2258, 0x88);
        assert_eq!(read_mmio(&mut chip, 0x230C), 0x77, "$00:6000 -> BW-RAM[0]");
    }

    /// The bit reader's view (`Sa1Chip::vbr_read`) deliberately does NOT map
    /// MMIO: a stream cursor pointed at a register reads open bus (the CPU
    /// MDR, `0` on a fresh chip), not the register: the view never reaches
    /// `$2200-23FF`.
    #[test]
    fn varbit_vbrbus_excludes_mmio() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        // VC ($230E) reads 0x01 through the normal MMIO path.
        assert_eq!(read_mmio(&mut chip, 0x230E), 0x01);
        // Point the reader at $00:230E; its view returns open bus, not 0x01.
        set_vda(&mut chip, 0x00_230E);
        write_mmio(&mut chip, 0x2258, 0x88); // hl = 1, vb = 8
        assert_eq!(
            read_mmio(&mut chip, 0x230C),
            0x00,
            "VBRBus reads open bus at an MMIO address, not the register"
        );
    }

    /// Fixed mode advances the cursor on the VBD write, so writing a fresh width
    /// before each read walks a byte-packed ROM stream.
    #[test]
    fn varbit_fixed_mode_advances_on_vbd_write() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        set_vda(&mut chip, 0xC0_0000); // va = ROM offset 0
        write_mmio(&mut chip, 0x2258, 0x08); // fixed, vb = 8 -> va steps to ROM[1]
        assert_eq!(read_mmio(&mut chip, 0x230C), 0x01, "after write -> ROM[1]");
        write_mmio(&mut chip, 0x2258, 0x08); // -> ROM[2]
        assert_eq!(read_mmio(&mut chip, 0x230C), 0x02, "next write -> ROM[2]");
    }

    /// The reader cursor / width / mode survive a savestate round-trip: a
    /// restored chip resumes the stream at the same ROM byte.
    #[test]
    fn varbit_savestate_round_trips_through_mmio() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        set_vda(&mut chip, 0xC0_0005); // va -> ROM[5]
        write_mmio(&mut chip, 0x2258, 0x88); // hl = 1, vb = 8
        let saved = chip.serialize();

        let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        restored.restore(&saved, NTSC_HZ);
        assert_eq!(read_mmio(&mut restored, 0x230C), 0x05, "resumes at ROM[5]");
        assert!(restored.varbit.advances_on_read(), "advance mode restored");
        assert_eq!(restored.varbit.width(), 8, "field width restored");
        assert_eq!(restored.serialize(), saved, "resave is byte-identical");
    }

    /// Reset returns the reader to power-on: the cursor, width, and mode clear.
    #[test]
    fn varbit_reset_clears_reader() {
        let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
        set_vda(&mut chip, 0xC0_1234);
        write_mmio(&mut chip, 0x2258, 0x8C); // hl = 1, vb = 12
        assert_ne!(chip.varbit.address(), 0);
        chip.reset(NTSC_HZ);
        assert_eq!(chip.varbit.address(), 0, "cursor cleared");
        assert_eq!(chip.varbit.width(), 0, "width cleared");
        assert!(!chip.varbit.advances_on_read(), "advance mode cleared");
    }
}
