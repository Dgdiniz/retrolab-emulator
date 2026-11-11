//! Sega Virtua Processor (SVP) coprocessor, memory controller + SSP1601.
//!
//! The SVP couples a Samsung SSP1601 DSP with a 68000-visible memory
//! controller. This chip owns both: the controller (linear DRAM, the two
//! read-only cell-arrange DRAM views, and the `0xA15000` XST/PM0 command
//! mailbox) and a live [`Ssp1601`] interpreter that executes the DSP program
//! embedded in the cartridge ROM. The DSP reaches the shared DRAM and the
//! cartridge ROM through the injected [`SvpBusHost`], so the interpreter stays
//! a pure, console-agnostic crate.
//!
//! What each 68000 window does, the sync-word wake on a DRAM write, the two
//! cell-arrange address permutations, the XST/PM0 mailbox, follows the SVP
//! memory handlers Gražvydas "notaz" Ignotas wrote for PicoDrive
//! (`pico/carthw/svp/memory.c`, BSD 3-Clause; the notice is at the end of
//! `LICENSE`), restated for a bus
//! that delivers bytes. The chip receives already-resolved *chip offsets* from
//! the cartridge's [`MapPlan`]; each 68000 window is assigned a distinct
//! chip-base (in `presets::svp_plan`) so a single `read`/`write` can decode
//! which window an access belongs to:
//!
//! | chip offset               | window                                    |
//! |---------------------------|-------------------------------------------|
//! | `0 .. SVP_DRAM_LEN`        | linear DRAM (68000 banks `0x30`/`0x31`)   |
//! | `SVP_MAILBOX_BASE + 0..B`  | XST/PM0 mailbox (`0xA15000-0xA1500B`)     |
//! | `SVP_CELL1_BASE + 0..FFFF` | cell-arrange view 1 (`0x390000`, RO)      |
//! | `SVP_CELL2_BASE + 0..FFFF` | cell-arrange view 2 (`0x3A0000`, RO)      |
//!
//! **Timing.** The SSP runs decoupled from the 68000. Like the SNES DSP-1,
//! the chip catches the DSP up to the current master cycle at the top of every
//! 68000 access (and once more per frame from `catch_up_chip`), so a 68000
//! spin-wait that polls a DRAM sync word actually advances the DSP that clears
//! it, within a single frame. The budget is 800 SSP cycles per 3420-mclk
//! scanline, under the interpreter's one-cycle-per-instruction accounting
//! (see `SSP_CYCLES_PER_SCANLINE`).
//!
//! [`Coprocessor`]: retro_core::cartridge::Coprocessor

use std::sync::Arc;

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};
use ssp1601::{
    Ssp1601, SspHost, IRAM_WORDS, RAM_WORDS, SSP_A, SSP_PM0, SSP_WAIT_30FE06, SSP_WAIT_30FE08,
    SSP_WAIT_PM0, SSP_XST,
};

use crate::cartridge::composable::presets::{SVP_CELL1_BASE, SVP_CELL2_BASE, SVP_MAILBOX_BASE};
use crate::cartridge::composable::savestate::{GenesisCoprocessorState, SvpState};

/// Size of the 68000-visible SVP DRAM (`0x300000-0x31FFFF`, banks
/// `0x30`/`0x31`).
pub const SVP_DRAM_LEN: usize = 0x2_0000;

/// DRAM byte offset of the `0x30FE06` sync word (68k->SSP job handshake).
const SYNC_30FE06: usize = 0xFE06;
/// DRAM byte offset of the `0x30FE08` sync word (68k->SSP job number).
const SYNC_30FE08: usize = 0xFE08;

/// SSP cycle budget per scanline. With one cycle per instruction this is
/// close to 800 instructions a line, the figure Genesis Plus GX runs;
/// PicoDrive runs 850 cycles and charges one to three per instruction. In a
/// Virtua Racing race the DSP spends well under the budget either way and
/// idles on its sync words for the rest of the line.
const SSP_CYCLES_PER_SCANLINE: u64 = 800;
/// Master clocks per Genesis scanline (NTSC), the catch-up denominator.
const MCLK_PER_SCANLINE: u64 = 3420;
/// The Genesis 68000 runs at master clock / 7. The `master_cycle` threaded
/// through the bus (and `catch_up_chip`) is a **68K-cycle** counter, so it is
/// scaled back up to master clocks before applying the `800 / 3420` SSP ratio;
/// otherwise the DSP would run ~7x too slow and never finish a job.
const CPU_DIVIDER: u64 = 7;

/// The Sega Virtua Processor coprocessor.
pub struct SvpCoprocessor {
    /// Cartridge ROM, shared via `Arc`. The SSP1601 program lives at
    /// `0x800..` and is copied into the DSP's instruction memory at reset; the
    /// DSP also reads model data straight out of it through the PMAC engine.
    rom: Arc<[u8]>,
    /// DRAM shared between the 68000 and the DSP (banks `0x30`/`0x31`),
    /// big-endian bytes.
    dram: Vec<u8>,
    /// The live SSP1601 DSP interpreter. Owns the register file, internal RAM,
    /// stack, PMAC, instruction memory, and the mailbox registers
    /// (`SSP_XST`/`SSP_PM0`) + wait flags (`emu_status`).
    ssp: Ssp1601,
    accesses: u64,
    cycles: u64,
    /// 68K-cycle anchor for the DSP catch-up (`None` until the first).
    last_master_cycle: Option<u64>,
    /// Fractional master-cycle->SSP-cycle carry.
    ssp_accum: u64,
    info: ExpansionChipInfo,
}

/// Lends the SVP's shared DRAM and cartridge ROM to the SSP1601 as its
/// [`SspHost`]. Word-addressed, converting to/from the big-endian byte stores.
struct SvpBusHost<'a> {
    dram: &'a mut [u8],
    rom: &'a [u8],
}

impl SspHost for SvpBusHost<'_> {
    fn read_dram(&self, word_addr: u16) -> u16 {
        let off = (word_addr as usize) * 2;
        let hi = self.dram.get(off).copied().unwrap_or(0);
        let lo = self.dram.get(off + 1).copied().unwrap_or(0);
        (u16::from(hi) << 8) | u16::from(lo)
    }

    fn write_dram(&mut self, word_addr: u16, value: u16) {
        let off = (word_addr as usize) * 2;
        if off + 1 < self.dram.len() {
            self.dram[off] = (value >> 8) as u8;
            self.dram[off + 1] = (value & 0xFF) as u8;
        }
    }

    fn read_cart_rom(&self, word_addr: u32) -> u16 {
        let off = (word_addr as usize) * 2;
        let hi = self.rom.get(off).copied().unwrap_or(0);
        let lo = self.rom.get(off + 1).copied().unwrap_or(0);
        (u16::from(hi) << 8) | u16::from(lo)
    }
}

impl SvpCoprocessor {
    /// Build the SVP over the cartridge `rom`. `master_clock_hz` goes unused:
    /// the catch-up runs on fixed per-line ratios.
    /// Cold-resets the SSP1601, which copies the program ROM (`rom[0x800..]`)
    /// into the DSP's instruction memory and points its PC at `0x400`.
    pub fn new(rom: Arc<[u8]>, _master_clock_hz: u64) -> Self {
        let mut ssp = Ssp1601::new();
        ssp.cold_reset(&rom);
        Self {
            rom,
            dram: vec![0u8; SVP_DRAM_LEN],
            ssp,
            accesses: 0,
            cycles: 0,
            last_master_cycle: None,
            ssp_accum: 0,
            info: ExpansionChipInfo {
                name: "SVP".to_string(),
                chip_type: ExpansionChipType::SVP,
                description: "Sega Virtua Processor (SSP1601 DSP)".to_string(),
                features: vec![
                    "3D calculations".to_string(),
                    "SSP1601 DSP core".to_string(),
                ],
            },
        }
    }

    /// Length of the cartridge ROM the SVP was handed (diagnostic).
    pub fn rom_len(&self) -> usize {
        self.rom.len()
    }

    /// Advance the SSP1601 to `master_cycle`, running the DSP for the number of
    /// cycles proportional to the elapsed master clocks
    /// (`SSP_CYCLES_PER_SCANLINE` per 3420-mclk scanline). Monotonic and
    /// self-anchoring: the first call only sets the anchor; a target at or
    /// behind it is a no-op. The interpreter runs nothing while a wait flag is
    /// pending and yields as soon as one is set, so a huge (per-frame) budget
    /// is safe. Runs with split borrows so the DSP can reach DRAM/ROM.
    fn run_ssp(&mut self, master_cycle: u64) {
        let last = match self.last_master_cycle {
            Some(l) => l,
            None => {
                self.last_master_cycle = Some(master_cycle);
                return;
            }
        };
        if master_cycle <= last {
            return;
        }
        let delta = master_cycle - last;
        self.last_master_cycle = Some(master_cycle);
        // `delta` is in 68K cycles; convert to master clocks (x7) before the
        // 800-SSP-cycles-per-3420-master-clock scanline ratio.
        let master_delta = delta.saturating_mul(CPU_DIVIDER);
        self.ssp_accum = self
            .ssp_accum
            .saturating_add(master_delta.saturating_mul(SSP_CYCLES_PER_SCANLINE));
        let budget = self.ssp_accum / MCLK_PER_SCANLINE;
        self.ssp_accum -= budget * MCLK_PER_SCANLINE;
        if budget == 0 {
            return;
        }
        let budget = budget.min(i32::MAX as u64) as i32;

        let Self { ssp, dram, rom, .. } = self;
        let mut host = SvpBusHost {
            dram: dram.as_mut_slice(),
            rom: rom.as_ref(),
        };
        let ran = ssp.run(budget, &mut host);
        self.cycles = self.cycles.wrapping_add(ran);
    }

    /// Read the 16-bit big-endian DRAM word at byte offset `off`.
    fn dram_word(&self, off: usize) -> u16 {
        let hi = self.dram.get(off).copied().unwrap_or(0);
        let lo = self.dram.get(off + 1).copied().unwrap_or(0);
        (u16::from(hi) << 8) | u16::from(lo)
    }

    /// Linear DRAM write with the `0x30FE06`/`0x30FE08` sync-word wake
    /// (`PicoWrite16_dram`): once either sync word is non-zero, the matching
    /// SSP wait flag clears so the DSP resumes its job loop.
    fn write_dram(&mut self, off: usize, value: u8) {
        if let Some(slot) = self.dram.get_mut(off) {
            *slot = value;
        }
        match off & !1 {
            SYNC_30FE06 if self.dram_word(SYNC_30FE06) != 0 => {
                self.ssp.emu_status &= !SSP_WAIT_30FE06;
            }
            SYNC_30FE08 if self.dram_word(SYNC_30FE08) != 0 => {
                self.ssp.emu_status &= !SSP_WAIT_30FE08;
            }
            _ => {}
        }
    }

    /// Read a byte from a cell-arranged DRAM view (68000 banks `0x39`/`0x3A`).
    /// `low` is the 16-bit address within the window; `cell2` selects the
    /// second permutation. The permutation acts on the word index
    /// (`PicoRead16_svpca1`/`_svpca2`), and a byte read returns the high byte
    /// (even address) or low byte (odd) of the word it selects.
    fn read_cell(&self, low: u32, cell2: bool) -> u8 {
        let a = low & 0xFFFF;
        let word_index = a >> 1;
        let permuted = if cell2 {
            (word_index & 0x7801) | ((word_index & 0x1e) << 6) | ((word_index & 0x7e0) >> 4)
        } else {
            (word_index & 0x7001) | ((word_index & 0x3e) << 6) | ((word_index & 0xfc0) >> 5)
        } as usize;
        let word = self.dram_word(permuted << 1);
        if a & 1 == 1 {
            (word & 0xFF) as u8
        } else {
            (word >> 8) as u8
        }
    }

    /// 68000-side mailbox read (`0xA15000-0xA1500B`). `reg` is the byte index
    /// within the window. `0x00-0x03` return a byte of `XST` (even = high,
    /// odd = low); `0x04-0x05` return `PM0` and clear its bit 0 ("SSP wrote"),
    /// as `PicoRead16_svpr` does (both mailbox registers live in the SSP
    /// register file, so the 68000 reads/writes them straight through).
    fn read_mailbox(&mut self, reg: u32) -> u8 {
        match reg {
            0x00..=0x03 => byte_of(self.ssp.reg16(SSP_XST), reg),
            0x04 | 0x05 => {
                let pm0 = self.ssp.reg16(SSP_PM0);
                self.ssp.set_reg16(SSP_PM0, pm0 & !1);
                byte_of(pm0, reg)
            }
            // 0xA15006 halt-during-DMA is not modelled (optional, per svpdoc).
            _ => 0xFF,
        }
    }

    /// 68000-side mailbox write. A write to the XST window (`0x00-0x03`) sets
    /// the matching `XST` byte, raises `PM0` bit 1 ("68k wrote"), and clears
    /// `SSP_WAIT_PM0` so the DSP wakes (`PicoWrite16_svpr`).
    fn write_mailbox(&mut self, reg: u32, value: u8) {
        if let 0x00..=0x03 = reg {
            let cur = self.ssp.reg16(SSP_XST);
            let xst = if reg & 1 == 1 {
                (cur & 0xFF00) | u16::from(value)
            } else {
                (cur & 0x00FF) | (u16::from(value) << 8)
            };
            self.ssp.set_reg16(SSP_XST, xst);
            let pm0 = self.ssp.reg16(SSP_PM0) | 2;
            self.ssp.set_reg16(SSP_PM0, pm0);
            self.ssp.emu_status &= !SSP_WAIT_PM0;
        }
    }
}

/// Select the high (even `reg`) or low (odd `reg`) byte of a 16-bit register,
/// matching the 68000's big-endian byte order.
fn byte_of(value: u16, reg: u32) -> u8 {
    if reg & 1 == 1 {
        (value & 0xFF) as u8
    } else {
        (value >> 8) as u8
    }
}

impl Coprocessor for SvpCoprocessor {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, master_cycle: u64) -> u8 {
        self.run_ssp(master_cycle);
        self.accesses += 1;
        let off = offset as usize;
        if off < SVP_DRAM_LEN {
            return self.dram.get(off).copied().unwrap_or(0xFF);
        }
        if let Some(reg) = offset.checked_sub(SVP_MAILBOX_BASE) {
            if reg <= 0x0B {
                return self.read_mailbox(reg);
            }
        }
        if let Some(a) = offset.checked_sub(SVP_CELL1_BASE) {
            if a <= 0xFFFF {
                return self.read_cell(a, false);
            }
        }
        if let Some(a) = offset.checked_sub(SVP_CELL2_BASE) {
            if a <= 0xFFFF {
                return self.read_cell(a, true);
            }
        }
        0xFF
    }

    fn write(&mut self, offset: u32, value: u8, master_cycle: u64) {
        self.run_ssp(master_cycle);
        self.accesses += 1;
        let off = offset as usize;
        if off < SVP_DRAM_LEN {
            self.write_dram(off, value);
            return;
        }
        if let Some(reg) = offset.checked_sub(SVP_MAILBOX_BASE) {
            if reg <= 0x0B {
                self.write_mailbox(reg, value);
            }
        }
        // Cell-arrange views are read-only; their writes are dropped.
    }

    fn catch_up(&mut self, master_cycle: u64) {
        self.run_ssp(master_cycle);
    }

    fn reset(&mut self, _master_clock_hz: u64) {
        self.dram.fill(0);
        self.ssp.cold_reset(&self.rom);
        self.accesses = 0;
        self.cycles = 0;
        self.last_master_cycle = None;
        self.ssp_accum = 0;
    }

    fn serialize(&self) -> Vec<u8> {
        let state = GenesisCoprocessorState::Svp(SvpState {
            dram: self.dram.clone(),
            gr: self.ssp.gr,
            ram: self.ssp.ram.to_vec(),
            ptr: self.ssp.ptr,
            stack: self.ssp.stack,
            pmac: self.ssp.pmac,
            emu_status: self.ssp.emu_status,
            iram: self.ssp.iram_rom[0..IRAM_WORDS].to_vec(),
            cycles: self.cycles,
            last_master_cycle: self.last_master_cycle,
            ssp_accum: self.ssp_accum,
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        if let Ok(GenesisCoprocessorState::Svp(s)) =
            bincode::deserialize::<GenesisCoprocessorState>(data)
        {
            if s.dram.len() == self.dram.len() {
                self.dram = s.dram;
            }
            // Rebuild instruction memory from the cartridge, then overlay the
            // saved DSP state (registers, internal RAM, stack, PMAC, IRAM).
            self.ssp.cold_reset(&self.rom);
            self.ssp.gr = s.gr;
            if s.ram.len() == RAM_WORDS {
                self.ssp.ram.copy_from_slice(&s.ram);
            }
            self.ssp.ptr = s.ptr;
            self.ssp.stack = s.stack;
            self.ssp.pmac = s.pmac;
            self.ssp.emu_status = s.emu_status;
            if s.iram.len() == IRAM_WORDS {
                self.ssp.iram_rom[0..IRAM_WORDS].copy_from_slice(&s.iram);
            }
            self.cycles = s.cycles;
            self.last_master_cycle = s.last_master_cycle;
            self.ssp_accum = s.ssp_accum;
        }
    }

    fn debug_state(&self) -> ExpansionChipState {
        // Internal RAM (512 words) surfaced as big-endian bytes for the hex
        // viewer.
        let mut internal = Vec::with_capacity(RAM_WORDS * 2);
        for &w in self.ssp.ram.iter() {
            internal.push((w >> 8) as u8);
            internal.push((w & 0xFF) as u8);
        }
        ExpansionChipState {
            chip_type: ExpansionChipType::SVP,
            registers: vec![
                ChipRegister {
                    address: 0xA15000,
                    name: "XST".to_string(),
                    value: u32::from(self.ssp.reg16(SSP_XST)),
                    description: "Command/result mailbox (SSP XST)".to_string(),
                },
                ChipRegister {
                    address: 0xA15004,
                    name: "PM0".to_string(),
                    value: u32::from(self.ssp.reg16(SSP_PM0)),
                    description: "Mailbox status (PM0): bit0 SSP-wrote, bit1 68k-wrote".to_string(),
                },
                ChipRegister {
                    address: 0,
                    name: "PC".to_string(),
                    value: u32::from(self.ssp.program_counter()),
                    description: "SSP1601 program counter (word offset)".to_string(),
                },
                ChipRegister {
                    address: 0,
                    name: "A".to_string(),
                    value: self.ssp.gr[SSP_A],
                    description: "SSP1601 accumulator (32-bit)".to_string(),
                },
                ChipRegister {
                    address: 0,
                    name: "STATUS".to_string(),
                    value: self.ssp.emu_status,
                    description: "SSP wait-loop / PMC flags".to_string(),
                },
                ChipRegister {
                    address: 0,
                    name: "CYCLES".to_string(),
                    value: self.cycles as u32,
                    description: "SSP1601 cycle counter".to_string(),
                },
            ],
            internal_ram: Some(internal),
            program_counter: Some(u32::from(self.ssp.program_counter())),
            cycle_count: self.cycles,
        }
    }

    fn label(&self) -> &str {
        &self.info.name
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom() -> Arc<[u8]> {
        Arc::from(vec![0xA5u8; 0x200000].into_boxed_slice())
    }

    #[test]
    fn dram_read_write() {
        let mut svp = SvpCoprocessor::new(rom(), 53_693_175);
        svp.write(0x10, 0x42, 0);
        svp.write(0x1FFFF, 0x99, 0);
        assert_eq!(svp.read(0x10, 0), 0x42);
        assert_eq!(svp.read(0x1FFFF, 0), 0x99);
        // Between the DRAM top and the mailbox base is open bus.
        assert_eq!(svp.read(0x2FFFF, 0), 0xFF, "out of DRAM -> open bus");
        assert_eq!(svp.rom_len(), 0x200000);
    }

    #[test]
    fn mailbox_handshake_round_trips() {
        let mut svp = SvpCoprocessor::new(rom(), 53_693_175);
        // Pretend the SSP is waiting for a command.
        svp.ssp.emu_status = SSP_WAIT_PM0;

        // 68k writes a word (0xBEEF) to the XST mailbox as two byte writes.
        svp.write(SVP_MAILBOX_BASE, 0xBE, 0); // high byte (0xA15000)
        svp.write(SVP_MAILBOX_BASE + 1, 0xEF, 0); // low byte (0xA15001)
        assert_eq!(svp.ssp.reg16(SSP_XST), 0xBEEF);
        // The write raised PM0 bit 1 and woke the SSP.
        assert_eq!(svp.ssp.reg16(SSP_PM0) & 0x2, 0x2, "68k-wrote flag set");
        assert_eq!(svp.ssp.emu_status & SSP_WAIT_PM0, 0, "SSP wait cleared");

        // Reading XST back returns high then low byte.
        assert_eq!(svp.read(SVP_MAILBOX_BASE, 0), 0xBE);
        assert_eq!(svp.read(SVP_MAILBOX_BASE + 1, 0), 0xEF);

        // Reading PM0 (0xA15004) returns it and clears bit 0.
        let pm0 = svp.ssp.reg16(SSP_PM0) | 1; // pretend the SSP set "SSP-wrote"
        svp.ssp.set_reg16(SSP_PM0, pm0);
        let pm0_hi = svp.read(SVP_MAILBOX_BASE + 4, 0);
        assert_eq!(pm0_hi, 0x00, "PM0 high byte");
        let pm0_lo = svp.read(SVP_MAILBOX_BASE + 5, 0);
        // The first PM0 read already cleared bit 0.
        assert_eq!(pm0_lo & 1, 0, "PM0 bit0 cleared on read");
    }

    #[test]
    fn cell_arrange_view_reads_permuted_dram() {
        let mut svp = SvpCoprocessor::new(rom(), 53_693_175);
        // Cell view 1 at low address 0 maps to the permuted address 0
        // (all mask/shift terms are 0), so it reads dram[0] (high byte).
        svp.write(0, 0x12, 0);
        svp.write(1, 0x34, 0);
        assert_eq!(svp.read(SVP_CELL1_BASE, 0), 0x12, "cell1 even -> high byte");
        assert_eq!(
            svp.read(SVP_CELL1_BASE + 1, 0),
            0x34,
            "cell1 odd -> low byte"
        );
        // Cell view 2 at low address 0 likewise reads dram[0].
        assert_eq!(svp.read(SVP_CELL2_BASE, 0), 0x12, "cell2 even -> high byte");
        // A cell-window write is ignored (read-only view).
        svp.write(SVP_CELL1_BASE, 0xEE, 0);
        assert_eq!(svp.read(0, 0), 0x12, "cell write did not touch DRAM");
    }

    #[test]
    fn cell_arrange_permutation_matches_reference() {
        let mut svp = SvpCoprocessor::new(rom(), 53_693_175);
        // Pick a low address whose permutation is non-trivial and seed the
        // permuted DRAM word so the view returns it. View 1 moves word-index
        // bits 6-11 down by five: byte 0x1F80 is word 0xFC0, which lands on
        // word 0x7E, byte 0xFC.
        let a: u32 = 0x1F80;
        let permuted: u32 = 0xFC;
        svp.write(permuted, 0xAB, 0);
        svp.write(permuted + 1, 0xCD, 0);
        assert_eq!(svp.read(SVP_CELL1_BASE + a, 0), 0xAB);
        assert_eq!(svp.read(SVP_CELL1_BASE + a + 1, 0), 0xCD);

        // View 2 moves word-index bits 5-10 down by four and bits 1-4 up by
        // six: byte 0x07FE is word 0x3FF, which lands on word 0x7BF, byte
        // 0xF7E.
        let a: u32 = 0x07FE;
        let permuted: u32 = 0x0F7E;
        svp.write(permuted, 0x12, 0);
        svp.write(permuted + 1, 0x34, 0);
        assert_eq!(svp.read(SVP_CELL2_BASE + a, 0), 0x12);
        assert_eq!(svp.read(SVP_CELL2_BASE + a + 1, 0), 0x34);
    }

    #[test]
    fn dram_sync_word_clears_wait_flag() {
        let mut svp = SvpCoprocessor::new(rom(), 53_693_175);
        svp.ssp.emu_status = SSP_WAIT_30FE06 | SSP_WAIT_30FE08;
        // A zero write does not clear the flag.
        svp.write(SYNC_30FE06 as u32, 0x00, 0);
        assert_eq!(svp.ssp.emu_status & SSP_WAIT_30FE06, SSP_WAIT_30FE06);
        // A non-zero sync word clears the matching wait flag.
        svp.write(SYNC_30FE06 as u32 + 1, 0x01, 0);
        assert_eq!(svp.ssp.emu_status & SSP_WAIT_30FE06, 0);
        assert_eq!(
            svp.ssp.emu_status & SSP_WAIT_30FE08,
            SSP_WAIT_30FE08,
            "the other wait flag is untouched"
        );
        // The job-number sync word clears its own flag.
        svp.write(SYNC_30FE08 as u32, 0x05, 0);
        assert_eq!(svp.ssp.emu_status & SSP_WAIT_30FE08, 0);
    }

    #[test]
    fn savestate_round_trips_dram_and_ssp_state() {
        let mut a = SvpCoprocessor::new(rom(), 53_693_175);
        a.write(0x100, 0xAB, 0);
        a.write(SVP_MAILBOX_BASE, 0x12, 0);
        a.write(SVP_MAILBOX_BASE + 1, 0x34, 0);
        a.ssp.emu_status = SSP_WAIT_30FE08;
        // Poke some DSP internal state to prove the full register file persists.
        a.ssp.ram[7] = 0x1357;
        a.ssp.iram_rom[0x10] = 0x2468;
        a.cycles = 4242;
        let bytes = a.serialize();

        let mut b = SvpCoprocessor::new(rom(), 53_693_175);
        b.restore(&bytes, 53_693_175);
        assert_eq!(b.read(0x100, 0), 0xAB);
        assert_eq!(b.ssp.reg16(SSP_XST), 0x1234);
        assert_eq!(b.ssp.reg16(SSP_PM0) & 0x2, 0x2);
        assert_eq!(b.ssp.emu_status, SSP_WAIT_30FE08);
        assert_eq!(b.ssp.ram[7], 0x1357, "internal RAM restored");
        assert_eq!(b.ssp.iram_rom[0x10], 0x2468, "IRAM restored");
        assert_eq!(b.cycles, 4242);
    }

    #[test]
    fn reset_clears_dram_mailbox_and_counters() {
        let mut svp = SvpCoprocessor::new(rom(), 53_693_175);
        svp.write(0x100, 0xFF, 0);
        svp.write(SVP_MAILBOX_BASE, 0x55, 0);
        svp.ssp.emu_status = SSP_WAIT_PM0;
        svp.reset(53_693_175);
        assert_eq!(svp.read(0x100, 0), 0);
        assert_eq!(svp.ssp.reg16(SSP_XST), 0);
        assert_eq!(svp.ssp.reg16(SSP_PM0), 0);
        assert_eq!(svp.ssp.emu_status, 0);
        assert_eq!(svp.accesses(), 1, "reset zeroes counter, then one read");
        assert_eq!(svp.label(), "SVP");
        assert_eq!(svp.debug_state().chip_type, ExpansionChipType::SVP);
    }

    #[test]
    fn debug_state_exposes_mailbox_registers() {
        let mut svp = SvpCoprocessor::new(rom(), 53_693_175);
        svp.write(SVP_MAILBOX_BASE, 0xDE, 0);
        svp.write(SVP_MAILBOX_BASE + 1, 0xAD, 0);
        let state = svp.debug_state();
        let xst = state.registers.iter().find(|r| r.name == "XST").unwrap();
        assert_eq!(xst.value, 0xDEAD);
        assert!(state.registers.iter().any(|r| r.name == "PM0"));
        assert!(state.registers.iter().any(|r| r.name == "PC"));
        assert!(state.program_counter.is_some());
        assert!(state.internal_ram.is_some());
    }

    #[test]
    fn ssp_runs_and_advances_cycles_on_catch_up() {
        // A real program: five instructions that write a marker word to DRAM via
        // the PMAC, proving the DSP executes out of ROM and reaches DRAM.
        // Program words (big-endian) placed at cartridge byte 0x800 (= SSP
        // program word 0x400, where PC starts):
        //   ldi PMC, 0x0000  (address)
        //   ldi PMC, 0x0018  (mode = DRAM write)
        //   ld PM4, -        (blind write program)
        //   ldi A, 0xEEEE
        //   ld PM4, A        (dram word 0 = 0xEEEE)
        let program: [u16; 7] = [0x08E0, 0x0000, 0x08E0, 0x0018, 0x00C0, 0x0830, 0xEEEE];
        let mut bytes = vec![0u8; 0x200000];
        let extra = 0x00C3u16; // ld PM4, A
        let mut words: Vec<u16> = program.to_vec();
        words.push(extra);
        for (i, w) in words.iter().enumerate() {
            let off = 0x800 + i * 2;
            bytes[off] = (w >> 8) as u8;
            bytes[off + 1] = (w & 0xFF) as u8;
        }
        let rom: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
        let mut svp = SvpCoprocessor::new(rom, 53_693_175);

        // Anchor at cycle 0, then catch up 13680 68K cycles (95760 master
        // clocks, 28 scanlines): a budget of 22400 DSP cycles, ample for the program.
        svp.catch_up(0);
        svp.catch_up(MCLK_PER_SCANLINE * 4);
        assert!(svp.cycles > 0, "the DSP retired instructions");
        // The program wrote 0xEE into DRAM word 0 (high byte at offset 0).
        assert_eq!(svp.read(0, MCLK_PER_SCANLINE * 4), 0xEE);
    }

    #[test]
    fn no_battery_on_svp() {
        let svp = SvpCoprocessor::new(rom(), 53_693_175);
        assert!(svp.battery().is_none());
    }
}
