//! Composable multi-chip cartridge.
//!
//! [`ComposableCartridge`] is the SNES cartridge that hosts a [`ChipSet`]
//! of independent coprocessors alongside stock ROM + battery SRAM. The bus
//! dispatches chip MMIO to a specific `chip_id` (the id carried by the map
//! slot's [`MapTarget::ChipMmio`](super::MapTarget)) and this cartridge
//! forwards it straight to that chip in the set, so two chips at distinct
//! ids coexist with no bus change. ROM/SRAM behave exactly like a
//! [`PlainCartridge`](super::PlainCartridge).
//!
//! The ROM lives behind an `Arc<[u8]>` so a chip that needs the cartridge
//! ROM (an MMC or a decompressor) can be handed a cheap `Arc::clone` of the
//! same image the bus reads.
//!
//! Savestates bridge the generic opaque-bytes surface
//! ([`Coprocessor::serialize`]/[`restore`](Coprocessor::restore)) to the
//! console's typed [`CoprocessorState`]: each chip serializes a
//! `bincode`-encoded `CoprocessorState`, and this cartridge decodes it back.
//! The snapshot is a `Vec<(chip_id, CoprocessorState)>`, so every chip in the
//! set round-trips, a two-chip cartridge persists two entries.

use std::sync::Arc;

use retro_core::cartridge::{ChipSet, ExpansionChipState};

use crate::savestate::CoprocessorState;

use super::{MapPlan, SnesCartridge};

/// A cartridge composed of stock ROM + battery SRAM and a [`ChipSet`] of
/// independent coprocessors, each reachable at its own `chip_id`.
pub struct ComposableCartridge {
    /// ROM image, shared with any ROM-consuming chip via `Arc::clone`.
    rom: Arc<[u8]>,
    /// Battery-backed SRAM (empty when the cartridge has none).
    sram: Vec<u8>,
    /// Combined map plan: ROM/SRAM windows plus each chip's MMIO window.
    plan: MapPlan,
    /// The coprocessors, indexed by the `chip_id` the map plan carries.
    chips: ChipSet,
    /// The chip the single-value coprocessor accessors report
    /// (`coprocessor_label`/`accesses`/`debug_state`), the lowest-id chip
    /// present, i.e. the "primary" one for a summary view. The full set is
    /// enumerated by the plural surface (`coprocessors_savestate`,
    /// `coprocessors_debug_state`, `coprocessor_labels`).
    coproc_id: Option<u8>,
}

impl ComposableCartridge {
    /// Assemble a cartridge from its ROM image, SRAM, combined map plan, and
    /// populated chip set. The single-value coprocessor surface tracks the
    /// lowest-id chip in the set.
    pub fn new(rom: Arc<[u8]>, sram: Vec<u8>, plan: MapPlan, chips: ChipSet) -> Self {
        let coproc_id = chips.first_id();
        Self {
            rom,
            sram,
            plan,
            chips,
            coproc_id,
        }
    }
}

impl SnesCartridge for ComposableCartridge {
    fn map_plan(&self) -> &MapPlan {
        &self.plan
    }

    fn read_rom(&self, offset: u32) -> u8 {
        self.rom.get(offset as usize).copied().unwrap_or(0)
    }

    fn read_sram(&self, offset: u32) -> u8 {
        self.sram.get(offset as usize).copied().unwrap_or(0)
    }

    fn write_sram(&mut self, offset: u32, value: u8) {
        if let Some(slot) = self.sram.get_mut(offset as usize) {
            *slot = value;
        }
    }

    fn read_chip(&mut self, chip_id: u8, offset: u32, master_cycle: u64) -> u8 {
        // Missing chip -> open bus (0), matching the trait default and the
        // bus's MDR fold at the call site.
        self.chips.read(chip_id, offset, master_cycle).unwrap_or(0)
    }

    fn write_chip(&mut self, chip_id: u8, offset: u32, value: u8, master_cycle: u64) {
        self.chips.write(chip_id, offset, value, master_cycle);
    }

    fn catch_up_chip(&mut self, master_cycle: u64) {
        self.chips.catch_up_all(master_cycle);
    }

    fn reset_chip(&mut self, master_clock_hz: u64) {
        self.chips.reset_all(master_clock_hz);
    }

    fn observe_bus_write(&mut self, addr: u32, value: u8) {
        self.chips.observe_write(addr, value);
    }

    fn observes_bus(&self) -> bool {
        self.chips.has_observers()
    }

    fn has_irq(&self) -> bool {
        self.chips.has_irq()
    }

    fn coprocessor_label(&self) -> Option<&str> {
        self.chips.get(self.coproc_id?).map(|c| c.label())
    }

    fn coprocessor_accesses(&self) -> u64 {
        self.coproc_id
            .and_then(|id| self.chips.get(id))
            .map_or(0, |c| c.accesses())
    }

    fn coprocessors_savestate(&self) -> Vec<(u8, CoprocessorState)> {
        // Each chip serializes an opaque blob; a console chip encodes its
        // typed `CoprocessorState` with bincode. An empty blob means the
        // chip has nothing worth persisting (e.g. the DSP-1 idle stub) and
        // is dropped from the snapshot.
        self.chips
            .serialize()
            .into_iter()
            .filter(|(_, bytes)| !bytes.is_empty())
            .filter_map(|(id, bytes)| {
                bincode::deserialize::<CoprocessorState>(&bytes)
                    .ok()
                    .map(|state| (id, state))
            })
            .collect()
    }

    fn restore_coprocessors_savestate(
        &mut self,
        states: &[(u8, CoprocessorState)],
        master_clock_hz: u64,
    ) {
        // Re-encode each typed state to the opaque byte form the ChipSet
        // restore expects; it matches every blob to its chip by id.
        let blobs: Vec<(u8, Vec<u8>)> = states
            .iter()
            .filter_map(|(id, state)| bincode::serialize(state).ok().map(|bytes| (*id, bytes)))
            .collect();
        self.chips.restore(&blobs, master_clock_hz);
    }

    fn coprocessor_debug_state(&self) -> Option<ExpansionChipState> {
        self.chips.get(self.coproc_id?).map(|c| c.debug_state())
    }

    fn coprocessors_debug_state(&self) -> Vec<(u8, ExpansionChipState)> {
        self.chips.debug_states()
    }

    fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        self.chips.labels()
    }

    fn secondary_cpu_state(&self) -> Option<retro_core::types::CpuState> {
        self.chips.secondary_cpu_state()
    }

    fn step_secondary_cpu(&mut self) -> Option<retro_core::emulator::StepResult> {
        self.chips.step_secondary_cpu()
    }

    fn disassemble_secondary(&self, addr: u32, count: u32) -> Vec<retro_core::types::Instruction> {
        self.chips.disassemble_secondary(addr, count)
    }

    fn set_secondary_trace_enabled(&mut self, enabled: bool) {
        self.chips.set_secondary_trace_enabled(enabled);
    }

    fn take_secondary_trace(&mut self) -> Vec<u8> {
        self.chips.take_secondary_trace()
    }

    fn sram(&self) -> &[u8] {
        // A chip that owns a battery (the SPC7110's game SRAM, gated by its
        // `$4830` write-enable) is the source of truth for `.srm` persistence;
        // the cartridge's own SRAM buffer is unused in that case. Fall back to
        // the buffer for plain battery carts.
        match self.chips.battery() {
            Some(battery) => battery,
            None => &self.sram,
        }
    }

    fn restore_sram(&mut self, data: &[u8]) {
        // Route a loaded `.srm` (or savestate SRAM bytes) into the chip
        // battery when a chip owns one; otherwise into the cartridge buffer.
        if self.chips.has_battery() {
            self.chips.restore_battery(data);
            return;
        }
        let n = data.len().min(self.sram.len());
        self.sram[..n].copy_from_slice(&data[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::{
        BusSpeed, MapSlot, MapTarget, Sdd1Chip, SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE,
    };
    use crate::savestate::{CoprocessorState, Dsp1HleState};
    use retro_core::cartridge::{ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipType};

    /// A fully introspectable fake coprocessor. `read` answers
    /// `0x5A + offset` and counts accesses; `write` latches a `command`
    /// byte that doubles as the persisted-state marker. `serialize` encodes
    /// a `CoprocessorState::Dsp1Hle` carrying that command so the
    /// cartridge's typed savestate bridge is exercised without a real DSP-1.
    struct FakeChip {
        info: ExpansionChipInfo,
        label: String,
        accesses: u64,
        command: u8,
        reset_hz: u64,
    }

    impl FakeChip {
        fn new() -> Self {
            Self {
                info: ExpansionChipInfo {
                    name: "FAKE".to_string(),
                    chip_type: ExpansionChipType::DSP1,
                    description: String::new(),
                    features: Vec::new(),
                },
                label: "FAKE-CHIP".to_string(),
                accesses: 0,
                command: 0,
                reset_hz: 0,
            }
        }
    }

    impl Coprocessor for FakeChip {
        fn info(&self) -> &ExpansionChipInfo {
            &self.info
        }

        fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
            self.accesses += 1;
            0x5A_u8.wrapping_add(offset as u8)
        }

        fn write(&mut self, _offset: u32, value: u8, _master_cycle: u64) {
            self.accesses += 1;
            self.command = value;
        }

        fn reset(&mut self, master_clock_hz: u64) {
            self.reset_hz = master_clock_hz;
            self.accesses = 0;
            self.command = 0;
        }

        fn serialize(&self) -> Vec<u8> {
            if self.command == 0 {
                return Vec::new();
            }
            let state = CoprocessorState::Dsp1Hle(Dsp1HleState {
                command: self.command,
                ..Default::default()
            });
            bincode::serialize(&state).unwrap_or_default()
        }

        fn restore(&mut self, data: &[u8], master_clock_hz: u64) {
            self.reset_hz = master_clock_hz;
            if let Ok(CoprocessorState::Dsp1Hle(s)) = bincode::deserialize::<CoprocessorState>(data)
            {
                self.command = s.command;
            }
        }

        fn debug_state(&self) -> ExpansionChipState {
            ExpansionChipState {
                chip_type: self.info.chip_type,
                registers: vec![ChipRegister {
                    address: 0,
                    name: "command".to_string(),
                    value: u32::from(self.command),
                    description: String::new(),
                }],
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

    fn plan() -> MapPlan {
        let mut plan = MapPlan::new("composable-test");
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        plan
    }

    /// A cartridge whose single fake chip sits at `chip_id` (leaving lower
    /// ids empty) so the id-addressable dispatch is under test.
    fn cart_with_fake_at(chip_id: u8) -> ComposableCartridge {
        let mut rom = vec![0u8; 0x8000];
        rom[0x10] = 0x42;
        let mut chips = ChipSet::default();
        chips.insert(chip_id, Box::new(FakeChip::new()));
        ComposableCartridge::new(
            Arc::from(rom.into_boxed_slice()),
            vec![0u8; 0x800],
            plan(),
            chips,
        )
    }

    #[test]
    fn rom_and_sram_delegate_to_storage() {
        let mut c = cart_with_fake_at(1);
        assert_eq!(c.read_rom(0x10), 0x42);
        assert_eq!(c.read_rom(0x7FFF_FFFF), 0, "out-of-bounds ROM reads 0");
        assert_eq!(c.map_plan().name, "composable-test");

        c.write_sram(0, 0xAB);
        c.write_sram(0x7FF, 0xCD);
        assert_eq!(c.read_sram(0), 0xAB);
        assert_eq!(c.read_sram(0x7FF), 0xCD);
        assert_eq!(c.sram().len(), 0x800);

        // Bulk restore degrades gracefully on a size mismatch.
        c.restore_sram(&[0x11, 0x22]);
        assert_eq!(c.read_sram(0), 0x11);
        assert_eq!(c.read_sram(1), 0x22);
    }

    #[test]
    fn chip_dispatch_routes_by_id() {
        let mut c = cart_with_fake_at(1);
        // The chip is at id 1; id 0 is empty and reads open bus (0).
        assert_eq!(c.read_chip(0, 0, 100), 0);
        // id 1 hits the fake: read returns 0x5A + offset.
        assert_eq!(c.read_chip(1, 0, 100), 0x5A);
        assert_eq!(c.read_chip(1, 0x05, 100), 0x5F);
        // Writes route to id 1 too; a write to id 0 is dropped (no chip).
        c.write_chip(0, 0, 0x77, 100);
        c.write_chip(1, 0, 0x33, 100);
        assert_eq!(
            c.coprocessor_debug_state().unwrap().registers[0].value,
            0x33
        );
    }

    #[test]
    fn single_value_accessors_track_primary_chip() {
        let mut c = cart_with_fake_at(1);
        assert_eq!(c.coprocessor_label(), Some("FAKE-CHIP"));
        assert_eq!(c.coprocessor_accesses(), 0);
        c.read_chip(1, 0, 0);
        c.write_chip(1, 0, 0x01, 0);
        assert_eq!(c.coprocessor_accesses(), 2);
        let st = c.coprocessor_debug_state().expect("chip present");
        assert_eq!(st.chip_type, ExpansionChipType::DSP1);
    }

    #[test]
    fn plain_cartridge_has_no_coprocessor_surface() {
        let chips = ChipSet::default();
        let c = ComposableCartridge::new(
            Arc::from(vec![0u8; 16].into_boxed_slice()),
            Vec::new(),
            plan(),
            chips,
        );
        assert_eq!(c.coprocessor_label(), None);
        assert_eq!(c.coprocessor_accesses(), 0);
        assert!(c.coprocessor_debug_state().is_none());
        assert!(c.coprocessors_savestate().is_empty());
        assert!(c.coprocessors_debug_state().is_empty());
        assert!(c.coprocessor_labels().is_empty());
    }

    #[test]
    fn savestate_bridge_round_trips_typed_state() {
        let mut a = cart_with_fake_at(1);
        // No state written yet -> nothing to persist.
        assert!(a.coprocessors_savestate().is_empty());

        // Drive a command byte into the chip, then capture.
        a.write_chip(1, 0, 0x2A, 0);
        let saved = a.coprocessors_savestate();
        assert_eq!(saved.len(), 1, "one chip present -> one entry");
        let (id, ref state) = saved[0];
        assert_eq!(id, 1, "the entry is keyed by the chip's id");
        match state {
            CoprocessorState::Dsp1Hle(s) => assert_eq!(s.command, 0x2A),
            other => panic!("expected an HLE blob, got {other:?}"),
        }

        // Restore into a fresh cartridge and confirm the chip took the state.
        let mut b = cart_with_fake_at(1);
        b.restore_coprocessors_savestate(&saved, 21_477_272);
        assert_eq!(
            b.coprocessor_debug_state().unwrap().registers[0].value,
            0x2A
        );
        // And a re-capture reproduces the same typed blob.
        let recaptured = b.coprocessors_savestate();
        assert!(matches!(
            recaptured.as_slice(),
            [(1, CoprocessorState::Dsp1Hle(s))] if s.command == 0x2A
        ));
    }

    #[test]
    fn reset_chip_reaches_the_set() {
        let mut c = cart_with_fake_at(1);
        c.write_chip(1, 0, 0x50, 0);
        assert_eq!(c.coprocessor_accesses(), 1);
        c.reset_chip(21_477_272);
        assert_eq!(c.coprocessor_accesses(), 0);
        // State cleared by reset -> nothing to persist.
        assert!(c.coprocessors_savestate().is_empty());
    }

    /// A cartridge with two chips at distinct ids round-trips *both* through
    /// the multi-chip savestate bridge.
    #[test]
    fn savestate_round_trips_two_chips() {
        let mut chips = ChipSet::default();
        chips.insert(1, Box::new(FakeChip::new()));
        chips.insert(3, Box::new(FakeChip::new()));
        let mut a = ComposableCartridge::new(
            Arc::from(vec![0u8; 0x8000].into_boxed_slice()),
            vec![0u8; 0x800],
            plan(),
            chips,
        );

        // Drive a *different* command byte into each chip so the round-trip
        // has to keep them distinct (not collapse to one).
        a.write_chip(1, 0, 0x11, 0);
        a.write_chip(3, 0, 0x33, 0);

        let saved = a.coprocessors_savestate();
        assert_eq!(saved.len(), 2, "both chips persist an entry");
        assert!(
            matches!(&saved[0], (1, CoprocessorState::Dsp1Hle(s)) if s.command == 0x11),
            "first entry is chip 1 with its own command, got {saved:?}"
        );
        assert!(
            matches!(&saved[1], (3, CoprocessorState::Dsp1Hle(s)) if s.command == 0x33),
            "second entry is chip 3 with its own command, got {saved:?}"
        );

        // Restore into a fresh two-chip cartridge and confirm each chip took
        // its *own* state back, proving the id keying is honoured.
        let mut fresh = ChipSet::default();
        fresh.insert(1, Box::new(FakeChip::new()));
        fresh.insert(3, Box::new(FakeChip::new()));
        let mut b = ComposableCartridge::new(
            Arc::from(vec![0u8; 0x8000].into_boxed_slice()),
            vec![0u8; 0x800],
            plan(),
            fresh,
        );
        b.restore_coprocessors_savestate(&saved, 21_477_272);

        let debug = b.coprocessors_debug_state();
        assert_eq!(debug.len(), 2);
        assert_eq!(debug[0].0, 1);
        assert_eq!(debug[0].1.registers[0].value, 0x11);
        assert_eq!(debug[1].0, 3);
        assert_eq!(debug[1].1.registers[0].value, 0x33);

        // Labels enumerate both chips too.
        assert_eq!(
            b.coprocessor_labels(),
            vec![(1, "FAKE-CHIP".to_string()), (3, "FAKE-CHIP".to_string())]
        );
    }

    /// A *real* `Sdd1Chip` round-trips its mapping state through the
    /// cartridge's typed savestate bridge, the concrete counterpart to the
    /// `FakeChip` coverage above, exercising the `CoprocessorState::Sdd1`
    /// bincode path (MMC latches + enables + the `$43xx` DMA shadow).
    #[test]
    fn savestate_bridge_round_trips_real_sdd1_chip() {
        const HZ: u64 = 21_477_272;
        let rom: Arc<[u8]> = Arc::from(vec![0u8; 0x8000].into_boxed_slice());

        let sdd1_chips = || {
            let mut chips = ChipSet::default();
            chips.insert(SDD1_CHIP_ID, Box::new(Sdd1Chip::new(Arc::clone(&rom), HZ)));
            chips.add_observer(0x4300..=0x437F, SDD1_CHIP_ID);
            chips
        };

        let mut a = ComposableCartridge::new(Arc::clone(&rom), Vec::new(), plan(), sdd1_chips());
        // Drive the MMC latch + enables through the register window and one
        // channel's DMA shadow through the $43xx observer.
        a.write_chip(SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE + 0x4, 5, 0); // mmc[0] = 5
        a.write_chip(SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE, 0x81, 0); // $4800 sdd1_enable
        a.write_chip(SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE + 0x1, 0x01, 0); // xfer_enable
        for (reg, val) in [
            (0x2, 0x45),
            (0x3, 0x23),
            (0x4, 0xC1),
            (0x5, 0x34),
            (0x6, 0x12),
        ] {
            a.observe_bus_write(0x4300 + reg, val); // channel 0 src $C1_2345, size $1234
        }

        let saved = a.coprocessors_savestate();
        let orig = match saved.as_slice() {
            [(id, CoprocessorState::Sdd1(s))] if *id == SDD1_CHIP_ID => s.clone(),
            other => panic!("expected one S-DD1 entry, got {other:?}"),
        };
        assert_eq!(orig.mmc[0], 5 << 20);
        assert_eq!(orig.dma_addr[0], 0xC1_2345);
        assert_eq!(orig.dma_size[0], 0x1234);

        // Restore into a fresh cartridge and confirm the state came back.
        let mut b = ComposableCartridge::new(Arc::clone(&rom), Vec::new(), plan(), sdd1_chips());
        b.restore_coprocessors_savestate(&saved, HZ);
        let round = match b.coprocessors_savestate().as_slice() {
            [(_, CoprocessorState::Sdd1(s))] => s.clone(),
            other => panic!("expected one S-DD1 entry after restore, got {other:?}"),
        };
        assert_eq!(round, orig, "the S-DD1 state round-trips byte-for-byte");

        // The restored mapping surfaces in the debug state.
        let st = b.coprocessor_debug_state().expect("S-DD1 present");
        let mmc0 = st.registers.iter().find(|r| r.name == "MMC0").unwrap();
        assert_eq!(mmc0.value, 5);
        assert!(st
            .registers
            .iter()
            .any(|r| r.name == "DMA0" && r.value == 0xC1_2345));
    }
}
