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
//! [`crate::cartridge::presets::sa1_preset`] with a base that tells them apart:
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
    SourcePlace,
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

pub use crate::cartridge::chips::{
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

    /// The S-CPU's view of the chip read without a side effect: ROM through
    /// the Super MMC (with the vectors SCNT redirects), BW-RAM and I-RAM, as
    /// they stand. `None` for the registers, and for BW-RAM while the
    /// type-1 character conversion is armed: the chip makes those bytes as
    /// they are read.
    fn peek(&self, offset: u32) -> Option<u8> {
        if offset >= SA1_ROM_BASE {
            let snes_addr = offset - SA1_ROM_BASE;
            Some(
                self.interrupts
                    .scpu_vector_byte(snes_addr)
                    .unwrap_or_else(|| self.rom_via_mmc(snes_addr)),
            )
        } else if offset >= SA1_BWRAM_LINEAR_BASE {
            (!self.dma.conversion_armed())
                .then(|| self.bwram_linear_read(offset - SA1_BWRAM_LINEAR_BASE))
        } else if offset >= SA1_BWRAM_BASE {
            (!self.dma.conversion_armed()).then(|| self.bwram_window_read(offset - SA1_BWRAM_BASE))
        } else if offset >= SA1_IRAM_BASE {
            Some(self.iram_read(offset - SA1_IRAM_BASE))
        } else {
            None
        }
    }

    /// Where a byte the S-CPU reads comes from: both views of the BW-RAM
    /// are the cartridge's RAM; the ROM (through the Super MMC's banks),
    /// the I-RAM and the registers are the chip's.
    fn source_place(&self, offset: u32) -> SourcePlace {
        if (SA1_BWRAM_BASE..SA1_ROM_BASE).contains(&offset) {
            SourcePlace::CartridgeRam
        } else {
            SourcePlace::Coprocessor
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
mod tests;
