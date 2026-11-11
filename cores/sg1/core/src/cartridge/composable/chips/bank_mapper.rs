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
//! - **SEGA**: writes to `0xA130F0..0xA130F7` set the bank for slot
//!   `addr & 0x0F` (8 x 512 KiB slots; the mapper ignores `0xA130F8..0xA130FF`).
//!   Delivered via `observe_write` because `0xA130Fx` is outside the ROM
//!   window.
//! - **SSF2**: a single `0xA130F1` write toggles slot 7 between bank 7
//!   and bank 8; slots 0-6 stay linear. Also via `observe_write`.
//! - **Codemasters**: bank selects are *ROM-window* writes to
//!   `0x00000 / 0x08000 / 0x10000 / 0x18000`, so they arrive through the
//!   chip's `write` (the ROM window is this chip's MMIO).

use std::sync::Arc;

use retro_core::cartridge::{
    ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};

use crate::cartridge::composable::savestate::{
    BankMapperKind, BankMapperState, GenesisCoprocessorState,
};

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

    /// Resolve a ROM-window address to a bank number under the current table.
    fn bank_for(&self, addr: u32) -> u8 {
        let slot = ((addr >> 19) & 0x07) as usize;
        match self.kind {
            BankMapperKind::Sega => self.banks[slot],
            BankMapperKind::Ssf2 => {
                if slot == 7 {
                    self.banks[0] // the latched high bank
                } else {
                    slot as u8
                }
            }
            BankMapperKind::Codemasters => {
                let region = ((addr >> 19) & 0x03) as usize;
                self.banks[region]
            }
        }
    }

    /// Translate a ROM-window address to a physical ROM offset.
    fn physical_offset(&self, addr: u32) -> usize {
        let bank = self.bank_for(addr) as usize;
        bank * BANK_SIZE + (addr & BANK_OFFSET_MASK) as usize
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
                if (0xA130F0..=0xA130FF).contains(&addr) {
                    let slot = (addr & 0x0F) as usize;
                    if slot < 8 {
                        self.banks[slot] = self.clamp_bank(value);
                    }
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
        let state = GenesisCoprocessorState::BankMapper(BankMapperState {
            kind: self.kind,
            banks: self.banks.clone(),
        });
        bincode::serialize(&state).unwrap_or_default()
    }

    fn restore(&mut self, data: &[u8], _master_clock_hz: u64) {
        if let Ok(GenesisCoprocessorState::BankMapper(s)) =
            bincode::deserialize::<GenesisCoprocessorState>(data)
        {
            // Only accept a table for the same scheme and shape.
            if s.kind == self.kind && s.banks.len() == self.banks.len() {
                self.banks = s.banks;
            }
        }
    }

    fn debug_state(&self) -> ExpansionChipState {
        let registers = self
            .banks
            .iter()
            .enumerate()
            .map(|(i, &bank)| ChipRegister {
                address: 0xA130F0 + i as u32,
                name: format!("BANK{i}"),
                value: u32::from(bank),
                description: "ROM bank selection".to_string(),
            })
            .collect();
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

    #[test]
    fn sega_bank_switch_via_observe() {
        let mut m = BankMapperChip::sega(rom(16));
        // Switch slot 0 to bank 15 through 0xA130F0.
        m.observe_write(0xA130F0, 15);
        assert_eq!(m.read(0x000000, 0), 15);
        // Slot 1 unchanged.
        assert_eq!(m.read(0x080000, 0), 1);
        // Clamp beyond bank_count.
        m.observe_write(0xA130F0, 200);
        assert_eq!(m.read(0x000000, 0), 15, "clamped to last bank");
    }

    #[test]
    fn sega_savestate_round_trips() {
        let mut m = BankMapperChip::sega(rom(16));
        m.observe_write(0xA130F0, 9);
        m.observe_write(0xA130F7, 5);
        let bytes = m.serialize();

        let mut n = BankMapperChip::sega(rom(16));
        n.restore(&bytes, 0);
        assert_eq!(n.read(0x000000, 0), 9);
        assert_eq!(n.read(0x380000, 0), 5);
    }

    #[test]
    fn sega_reset_restores_linear() {
        let mut m = BankMapperChip::sega(rom(16));
        m.observe_write(0xA130F0, 9);
        m.reset(0);
        assert_eq!(m.read(0x000000, 0), 0);
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
        let m = BankMapperChip::sega(rom(8));
        let st = m.debug_state();
        assert_eq!(st.chip_type, ExpansionChipType::Custom(CUSTOM_SEGA));
        assert_eq!(st.registers.len(), 8);
        assert_eq!(st.registers[0].name, "BANK0");
        assert_eq!(m.label(), "SEGA mapper");
    }

    #[test]
    fn restore_rejects_mismatched_scheme() {
        let mut sega = BankMapperChip::sega(rom(8));
        sega.observe_write(0xA130F0, 3);
        let sega_bytes = sega.serialize();
        // A Codemasters chip must not accept a SEGA table.
        let mut cm = BankMapperChip::codemasters(rom(8));
        cm.restore(&sega_bytes, 0);
        assert_eq!(cm.read(0x000000, 0), 0, "unchanged by mismatched restore");
    }
}
