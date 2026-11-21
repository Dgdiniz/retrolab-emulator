//! Bank-switching mapper as a coprocessor (SEGA / SSF2 / Codemasters).
//!
//! Genesis ROMs larger than the 4 MiB direct window, and a few small
//! carts, page ROM through a bank table. Rather than a static map plan
//! (the banks change at runtime), the composable model represents this as
//! a *mapper-as-coprocessor*: the chip owns the cartridge ROM and answers
//! ROM-window reads (`0x000000-0x3FFFFF`) by translating the address
//! through its per-slot bank table. The plan therefore points the whole
//! ROM window at this chip's MMIO (`chip_id`, base 0) instead of at a
//! `Rom` target.
//!
//! The bank table is programmed differently per scheme:
//! - **SEGA**: 8 x 512 KiB slots. The bank registers are the odd
//!   addresses `0xA130F3` to `0xA130FF`, one for each of slots 1-7
//!   (`slot = (addr & 0x0F) >> 1`). Slot 0 has no register and always
//!   holds bank 0; `0xA130F1` is the board's SRAM switch and sets no bank,
//!   and the even addresses set none either. A 68000 word write reaches a
//!   register as its low byte: the bus sends the word's high byte to the
//!   even address and its low byte to the odd one. Delivered via
//!   `observe_write` because `0xA130Fx` is outside the ROM window.
//! - **SSF2**: a single `0xA130F1` write toggles slot 7 between bank 7
//!   and bank 8; slots 0-6 stay linear. Also via `observe_write`.
//! - **Codemasters**: bank selects are *ROM-window* writes to
//!   `0x00000 / 0x08000 / 0x10000 / 0x18000`, so they arrive through the
//!   chip's `write` (the ROM window is this chip's MMIO).

use std::sync::Arc;

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
    SourcePlace,
};

use crate::savestate::{BankMapperKind, BankMapperState, GenesisChipState};

/// 512 KiB per bank / slot.
const BANK_SIZE: usize = 0x80000;
/// Offset mask inside a 512 KiB bank.
const BANK_OFFSET_MASK: u32 = 0x7FFFF;

/// `ExpansionChipType::Custom` code for each banking scheme, so the
/// debugger can tell them apart.
const CUSTOM_SEGA: u16 = 0x0101;
const CUSTOM_SSF2: u16 = 0x0102;
const CUSTOM_CODEMASTERS: u16 = 0x0103;

/// A bank-switching mapper (SEGA / SSF2 / Codemasters) as a coprocessor.
pub struct BankMapperChip {
    rom: Arc<[u8]>,
    kind: BankMapperKind,
    /// Per-slot bank selection. SEGA: 8 entries; Codemasters: 4; SSF2: 1
    /// (the high bank latched into slot 7).
    banks: Vec<u8>,
    /// Number of 512 KiB banks in the real ROM (used to clamp selections).
    bank_count: usize,
    accesses: u64,
    info: ExpansionChipInfo,
    label: String,
}

impl BankMapperChip {
    /// Build a mapper of `kind` over `rom`.
    pub fn new(rom: Arc<[u8]>, kind: BankMapperKind) -> Self {
        let bank_count = rom.len().div_ceil(BANK_SIZE).max(1);
        let (banks, chip_custom, label) = match kind {
            BankMapperKind::Sega => {
                let mut banks = vec![0u8; 8];
                for (i, b) in banks.iter_mut().enumerate() {
                    *b = i.min(bank_count - 1) as u8;
                }
                (banks, CUSTOM_SEGA, "SEGA mapper")
            }
            // SSF2 keeps slots 0-6 linear implicitly; only the high bank in
            // slot 7 is stateful (7 or 8).
            BankMapperKind::Ssf2 => (vec![7u8], CUSTOM_SSF2, "SSF2 mapper"),
            BankMapperKind::Codemasters => {
                (vec![0u8, 1, 2, 3], CUSTOM_CODEMASTERS, "Codemasters mapper")
            }
        };
        Self {
            rom,
            kind,
            banks,
            bank_count,
            accesses: 0,
            info: ExpansionChipInfo {
                name: label.to_string(),
                chip_type: ExpansionChipType::Custom(chip_custom),
                description: "Genesis ROM bank mapper".to_string(),
                features: Vec::new(),
            },
            label: label.to_string(),
        }
    }

    /// Convenience constructor for the SEGA mapper.
    pub fn sega(rom: Arc<[u8]>) -> Self {
        Self::new(rom, BankMapperKind::Sega)
    }

    /// Convenience constructor for the SSF2 mapper.
    pub fn ssf2(rom: Arc<[u8]>) -> Self {
        Self::new(rom, BankMapperKind::Ssf2)
    }

    /// Convenience constructor for the Codemasters mapper.
    pub fn codemasters(rom: Arc<[u8]>) -> Self {
        Self::new(rom, BankMapperKind::Codemasters)
    }

    /// Translate a ROM-window address to a physical ROM offset.
    fn physical_offset(&self, addr: u32) -> usize {
        physical_offset(self.kind, &self.banks, addr)
    }

    /// Clamp a written bank number to the real ROM size.
    fn clamp_bank(&self, value: u8) -> u8 {
        (value as usize).min(self.bank_count - 1) as u8
    }

    /// The live bank table (for the debugger).
    pub fn banks(&self) -> &[u8] {
        &self.banks
    }
}

/// Resolve a ROM-window address to a bank number under the table `banks`
/// of a mapper of `kind`.
fn bank_for(kind: BankMapperKind, banks: &[u8], addr: u32) -> u8 {
    let slot = ((addr >> 19) & 0x07) as usize;
    match kind {
        BankMapperKind::Sega => banks[slot],
        BankMapperKind::Ssf2 => {
            if slot == 7 {
                banks[0] // the latched high bank
            } else {
                slot as u8
            }
        }
        BankMapperKind::Codemasters => {
            let region = ((addr >> 19) & 0x03) as usize;
            banks[region]
        }
    }
}

/// Translate a ROM-window address to a physical ROM offset under the table
/// `banks` of a mapper of `kind`.
fn physical_offset(kind: BankMapperKind, banks: &[u8], addr: u32) -> usize {
    let bank = bank_for(kind, banks, addr) as usize;
    bank * BANK_SIZE + (addr & BANK_OFFSET_MASK) as usize
}

/// The SEGA scheme's bank registers: the odd addresses from slot 1's to
/// slot 7's.
const SEGA_BANK_REGISTERS: std::ops::RangeInclusive<u32> = 0xA130F3..=0xA130FF;

/// The slot whose bank a write to `addr` sets under the SEGA scheme: 1-7
/// for the odd addresses `0xA130F3`-`0xA130FF`, `None` for any other
/// address (slot 0 has no register).
fn sega_bank_slot(addr: u32) -> Option<usize> {
    (SEGA_BANK_REGISTERS.contains(&addr) && addr & 1 == 1).then_some(((addr & 0x0F) >> 1) as usize)
}

/// The SEGA scheme's bank register of `slot` (1-7).
fn sega_bank_register(slot: usize) -> u32 {
    0xA130F1 + 2 * slot as u32
}

impl Coprocessor for BankMapperChip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
        self.accesses += 1;
        self.rom
            .get(self.physical_offset(offset))
            .copied()
            .unwrap_or(0xFF)
    }

    /// A ROM-window byte through the banks as they are set; nothing is
    /// counted.
    fn peek(&self, offset: u32) -> Option<u8> {
        Some(
            self.rom
                .get(self.physical_offset(offset))
                .copied()
                .unwrap_or(0xFF),
        )
    }

    /// The ROM offset a ROM-window read reaches through the banks as they
    /// are set; `Unknown` past the end of the ROM (the read is open bus).
    fn source_place(&self, offset: u32) -> SourcePlace {
        match u32::try_from(self.physical_offset(offset)) {
            Ok(physical) if (physical as usize) < self.rom.len() => SourcePlace::Rom(physical),
            _ => SourcePlace::Unknown,
        }
    }

    fn write(&mut self, offset: u32, value: u8, _master_cycle: u64) {
        self.accesses += 1;
        // Only Codemasters programs its banks via ROM-window writes; SEGA
        // and SSF2 ignore ROM-window writes (their registers are 0xA130Fx,
        // handled in observe_write). ROM is otherwise read-only.
        if self.kind == BankMapperKind::Codemasters {
            let region = match offset {
                0x00000 => Some(0),
                0x08000 => Some(1),
                0x10000 => Some(2),
                0x18000 => Some(3),
                _ => None,
            };
            if let Some(region) = region {
                self.banks[region] = self.clamp_bank(value);
            }
        }
    }

    fn observe_write(&mut self, addr: u32, value: u8) {
        match self.kind {
            BankMapperKind::Sega => {
                if let Some(slot) = sega_bank_slot(addr) {
                    self.banks[slot] = self.clamp_bank(value);
                }
            }
            BankMapperKind::Ssf2 => {
                if addr == 0xA130F1 {
                    // Toggle slot 7 between bank 7 and bank 8.
                    self.banks[0] = if value & 0x01 != 0 { 8 } else { 7 };
                }
            }
            BankMapperKind::Codemasters => {}
        }
    }

    fn reset(&mut self, _master_clock_hz: u64) {
        self.accesses = 0;
        match self.kind {
            BankMapperKind::Sega => {
                for (i, b) in self.banks.iter_mut().enumerate() {
                    *b = i.min(self.bank_count - 1) as u8;
                }
            }
            BankMapperKind::Ssf2 => self.banks[0] = 7,
            BankMapperKind::Codemasters => self.banks = vec![0, 1, 2, 3],
        }
    }

    fn serialize(&self) -> Vec<u8> {
        let state = GenesisChipState::BankMapper(BankMapperState {
            kind: self.kind,
            banks: self.banks.clone(),
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        if let Ok(GenesisChipState::BankMapper(s)) = bincode::deserialize::<GenesisChipState>(data)
        {
            // Only accept a table for the same scheme and shape.
            if s.kind == self.kind && s.banks.len() == self.banks.len() {
                self.banks = s.banks;
            }
        }
    }

    fn debug_state(&self) -> ExpansionChipState {
        let register = |address: u32, slot: usize, bank: u8| ChipRegister {
            address,
            name: format!("BANK{slot}"),
            value: u32::from(bank),
            description: "ROM bank selection".to_string(),
        };
        let registers = match self.kind {
            // Slot 0 has no register (it always holds bank 0): the list is
            // the seven registers, each at its address.
            BankMapperKind::Sega => self
                .banks
                .iter()
                .enumerate()
                .skip(1)
                .map(|(slot, &bank)| register(sega_bank_register(slot), slot, bank))
                .collect(),
            BankMapperKind::Ssf2 | BankMapperKind::Codemasters => self
                .banks
                .iter()
                .enumerate()
                .map(|(i, &bank)| register(0xA130F0 + i as u32, i, bank))
                .collect(),
        };
        ExpansionChipState {
            chip_type: self.info.chip_type,
            registers,
            internal_ram: None,
            program_counter: None,
            cycle_count: self.accesses,
        }
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom(banks: usize) -> Arc<[u8]> {
        // Fill each 512 KiB bank with its bank number so a read reveals the
        // physical bank it landed in.
        let mut r = vec![0u8; banks * BANK_SIZE];
        for b in 0..banks {
            r[b * BANK_SIZE] = b as u8;
        }
        Arc::from(r.into_boxed_slice())
    }

    #[test]
    fn sega_initial_mapping_is_linear() {
        let mut m = BankMapperChip::sega(rom(8));
        for slot in 0..8u32 {
            assert_eq!(m.read(slot * 0x80000, 0), slot as u8);
        }
    }

    /// The table a SEGA mapper starts with: slot `i` holds bank `i`.
    const LINEAR: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

    #[test]
    fn each_sega_bank_register_sets_its_slot() {
        for (register, slot) in [
            (0xA130F3, 1),
            (0xA130F5, 2),
            (0xA130F7, 3),
            (0xA130F9, 4),
            (0xA130FB, 5),
            (0xA130FD, 6),
            (0xA130FF, 7),
        ] {
            let mut m = BankMapperChip::sega(rom(16));
            let bank = 8 + slot as u8;
            m.observe_write(register, bank);
            let mut expected = LINEAR;
            expected[slot] = bank;
            assert_eq!(m.banks(), expected, "{register:#08X}");
            assert_eq!(m.read(slot as u32 * 0x80000, 0), bank, "{register:#08X}");
        }
    }

    #[test]
    fn the_even_addresses_and_a130f1_set_no_bank() {
        let mut m = BankMapperChip::sega(rom(16));
        for register in (0xA130F0..=0xA130FE).step_by(2).chain([0xA130F1]) {
            m.observe_write(register, 12);
        }
        assert_eq!(m.banks(), LINEAR);

        // A word write as the bus delivers it: the high byte at the even
        // address, then the low byte at the odd one, which sets the bank.
        m.observe_write(0xA130F4, 0x0B);
        assert_eq!(m.banks(), LINEAR, "the high byte sets nothing");
        m.observe_write(0xA130F5, 0x0A);
        assert_eq!(m.read(0x100000, 0), 10, "the low byte sets slot 2");
    }

    #[test]
    fn slot_0_stays_bank_0() {
        let mut m = BankMapperChip::sega(rom(16));
        for register in 0xA130F0..=0xA130FF {
            m.observe_write(register, 15);
        }
        assert_eq!(m.banks(), [0, 15, 15, 15, 15, 15, 15, 15]);
        assert_eq!(m.read(0x000000, 0), 0);
        assert_eq!(m.source_place(0x01_2345), SourcePlace::Rom(0x01_2345));
        assert_eq!(m.source_place(0x07_FFFF), SourcePlace::Rom(0x07_FFFF));
    }

    #[test]
    fn a_bank_past_the_rom_is_clamped_to_its_last() {
        // 5 MiB, as Super Street Fighter II: banks 0-9.
        let mut m = BankMapperChip::sega(rom(10));
        m.observe_write(0xA130FD, 9);
        assert_eq!(m.read(0x300000, 0), 9, "the last bank, as written");
        m.observe_write(0xA130FD, 10);
        assert_eq!(m.read(0x300000, 0), 9, "one past it is the last bank");
        m.observe_write(0xA130FF, 200);
        assert_eq!(m.read(0x380000, 0), 9, "far past it too");
        assert_eq!(m.banks(), [0, 1, 2, 3, 4, 5, 9, 9]);
    }

    #[test]
    fn sega_savestate_round_trips() {
        let mut m = BankMapperChip::sega(rom(16));
        m.observe_write(0xA130F3, 9);
        m.observe_write(0xA130FF, 5);
        let bytes = m.serialize();

        let mut n = BankMapperChip::sega(rom(16));
        n.restore(&bytes, 0);
        assert_eq!(n.banks(), [0, 9, 2, 3, 4, 5, 6, 5]);
        assert_eq!(n.read(0x080000, 0), 9);
        assert_eq!(n.read(0x380000, 0), 5);
        assert_eq!(
            n.serialize(),
            bytes,
            "the restored table saves the same bytes"
        );
    }

    #[test]
    fn sega_reset_restores_linear() {
        let mut m = BankMapperChip::sega(rom(16));
        m.observe_write(0xA130F3, 9);
        m.reset(0);
        assert_eq!(m.banks(), LINEAR);
        assert_eq!(m.read(0x080000, 0), 1);
        assert_eq!(m.accesses(), 1, "reset zeroes the counter, then one read");
    }

    #[test]
    fn ssf2_toggles_slot7_high_bank() {
        // 9 banks (0-8) -> 4.5 MiB, enough for the bank-8 selection.
        let mut m = BankMapperChip::ssf2(rom(9));
        // Slot 7 initially bank 7.
        assert_eq!(m.read(0x380000, 0), 7);
        // Toggle to bank 8.
        m.observe_write(0xA130F1, 0x01);
        assert_eq!(m.read(0x380000, 0), 8);
        // Slots 0-6 remain linear.
        assert_eq!(m.read(0x000000, 0), 0);
        assert_eq!(m.read(0x300000, 0), 6);
        // Toggle back.
        m.observe_write(0xA130F1, 0x00);
        assert_eq!(m.read(0x380000, 0), 7);
    }

    #[test]
    fn codemasters_banks_via_rom_window_writes() {
        let mut m = BankMapperChip::codemasters(rom(8));
        // Initial linear.
        assert_eq!(m.read(0x000000, 0), 0);
        assert_eq!(m.read(0x080000, 0), 1);
        // Switch region 1 to bank 5 via a ROM-window write to 0x08000.
        m.write(0x08000, 5, 0);
        assert_eq!(m.read(0x080000, 0), 5);
        // Region 0 unchanged.
        assert_eq!(m.read(0x000000, 0), 0);
        // Codemasters ignores 0xA130Fx.
        m.observe_write(0xA130F0, 3);
        assert_eq!(m.read(0x000000, 0), 0);
    }

    #[test]
    fn debug_state_lists_banks_and_type() {
        let mut m = BankMapperChip::sega(rom(16));
        m.observe_write(0xA130FD, 11);
        let st = m.debug_state();
        assert_eq!(st.chip_type, ExpansionChipType::Custom(CUSTOM_SEGA));
        // Slot 0 has no register: the seven registers, at their addresses.
        let listed: Vec<(u32, &str, u32)> = st
            .registers
            .iter()
            .map(|r| (r.address, r.name.as_str(), r.value))
            .collect();
        assert_eq!(
            listed,
            [
                (0xA130F3, "BANK1", 1),
                (0xA130F5, "BANK2", 2),
                (0xA130F7, "BANK3", 3),
                (0xA130F9, "BANK4", 4),
                (0xA130FB, "BANK5", 5),
                (0xA130FD, "BANK6", 11),
                (0xA130FF, "BANK7", 7),
            ]
        );
        assert_eq!(m.label(), "SEGA mapper");
    }

    #[test]
    fn a_byte_s_source_is_the_rom_offset_the_chip_reads() {
        let mut sega = BankMapperChip::sega(rom(16));
        sega.observe_write(0xA130FD, 11);
        let mut ssf2 = BankMapperChip::ssf2(rom(9));
        ssf2.observe_write(0xA130F1, 0x01);
        let mut codemasters = BankMapperChip::codemasters(rom(8));
        codemasters.write(0x08000, 5, 0);
        for chip in [&mut sega, &mut ssf2, &mut codemasters] {
            let accesses = chip.accesses();
            for slot in 0..8u32 {
                let address = slot * 0x80000 + 0x1234;
                let physical = chip.physical_offset(address) as u32;
                assert_eq!(
                    chip.source_place(address),
                    SourcePlace::Rom(physical),
                    "slot {slot}"
                );
            }
            assert_eq!(
                chip.accesses(),
                accesses,
                "naming a source counts no access"
            );
        }
        assert_eq!(
            sega.source_place(0x30_0010),
            SourcePlace::Rom((11 * BANK_SIZE + 0x10) as u32)
        );
        assert_eq!(
            ssf2.source_place(0x38_0000),
            SourcePlace::Rom((8 * BANK_SIZE) as u32)
        );
    }

    #[test]
    fn a_bank_past_the_rom_is_no_source() {
        // A 4.5 MiB ROM: the SSF2 mapper's bank 8 is its last half bank.
        let mut ssf2 = BankMapperChip::ssf2(Arc::from(vec![0u8; 0x440000].into_boxed_slice()));
        ssf2.observe_write(0xA130F1, 0x01);
        assert_eq!(ssf2.source_place(0x38_0000), SourcePlace::Rom(0x400000));
        assert_eq!(ssf2.source_place(0x3C_0000), SourcePlace::Unknown);
        assert_eq!(ssf2.read(0x3C_0000, 0), 0xFF, "the read is open bus too");
    }

    #[test]
    fn restore_rejects_mismatched_scheme() {
        let mut sega = BankMapperChip::sega(rom(8));
        sega.observe_write(0xA130F3, 3);
        let sega_bytes = sega.serialize();
        // A Codemasters chip must not accept a SEGA table.
        let mut cm = BankMapperChip::codemasters(rom(8));
        cm.restore(&sega_bytes, 0);
        assert_eq!(cm.read(0x080000, 0), 1, "unchanged by mismatched restore");
    }
}
