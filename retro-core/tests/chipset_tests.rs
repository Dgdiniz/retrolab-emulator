//! Unit tests for the composable multi-chip surface (`Coprocessor` / `ChipSet`
//! / `ChipFactory`). Pure: no bus, no WASM, no OPFS, just a fake coprocessor
//! exercised through the public `ChipSet` API.

use retro_core::cartridge::*;
use std::sync::Arc;

const HZ: u64 = 21_477_272; // NTSC master clock (arbitrary anchor for tests)

/// A fully introspectable fake coprocessor. Every mutation lands in a field
/// that is observable through the public `ChipSet` API (`debug_states`,
/// `serialize`, `read`), so tests never need to reach into `ChipSet`'s private
/// `chips` vector.
struct FakeChip {
    info: ExpansionChipInfo,
    label: String,
    counter: u64,        // `write` accumulates the written value
    observed: u64,       // `observe_write` accumulates the observed value
    last_cycle: u64,     // master_cycle of the last read/write
    catch_up_cycle: u64, // master_cycle of the last `catch_up`
    reset_hz: u64,       // hz of the last `reset` / `restore`
    reset_count: u64,
    accesses: u64, // read + write count
}

impl FakeChip {
    fn new(chip_type: ExpansionChipType, label: &str) -> Self {
        Self {
            info: ExpansionChipInfo {
                name: label.to_string(),
                chip_type,
                description: String::new(),
                features: Vec::new(),
            },
            label: label.to_string(),
            counter: 0,
            observed: 0,
            last_cycle: 0,
            catch_up_cycle: 0,
            reset_hz: 0,
            reset_count: 0,
            accesses: 0,
        }
    }

    fn boxed(chip_type: ExpansionChipType, label: &str) -> Box<dyn Coprocessor> {
        Box::new(Self::new(chip_type, label))
    }
}

impl Coprocessor for FakeChip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, master_cycle: u64) -> u8 {
        self.accesses += 1;
        self.last_cycle = master_cycle;
        // Fold the offset in so the console-layer pass-through is observable.
        (self.counter as u8).wrapping_add(offset as u8)
    }

    fn write(&mut self, _offset: u32, value: u8, master_cycle: u64) {
        self.accesses += 1;
        self.last_cycle = master_cycle;
        self.counter = self.counter.wrapping_add(value as u64);
    }

    fn observe_write(&mut self, _addr: u32, value: u8) {
        self.observed = self.observed.wrapping_add(value as u64);
    }

    fn catch_up(&mut self, master_cycle: u64) {
        self.catch_up_cycle = master_cycle;
    }

    fn reset(&mut self, master_clock_hz: u64) {
        self.reset_hz = master_clock_hz;
        self.reset_count += 1;
        self.counter = 0;
        self.observed = 0;
    }

    fn serialize(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(32);
        v.extend_from_slice(&self.counter.to_le_bytes());
        v.extend_from_slice(&self.observed.to_le_bytes());
        v.extend_from_slice(&self.catch_up_cycle.to_le_bytes());
        v.extend_from_slice(&self.reset_count.to_le_bytes());
        v
    }

    fn restore(&mut self, data: &[u8], master_clock_hz: u64) {
        // Re-anchor the clock regardless of blob validity.
        self.reset_hz = master_clock_hz;
        // An incompatible blob is dropped silently (trait contract).
        if data.len() != 32 {
            return;
        }
        self.counter = u64::from_le_bytes(data[0..8].try_into().unwrap());
        self.observed = u64::from_le_bytes(data[8..16].try_into().unwrap());
        self.catch_up_cycle = u64::from_le_bytes(data[16..24].try_into().unwrap());
        self.reset_count = u64::from_le_bytes(data[24..32].try_into().unwrap());
    }

    fn debug_state(&self) -> ExpansionChipState {
        let reg = |name: &str, value: u64| ChipRegister {
            address: 0,
            name: name.to_string(),
            value: value as u32,
            description: String::new(),
        };
        ExpansionChipState {
            chip_type: self.info.chip_type,
            registers: vec![
                reg("counter", self.counter),
                reg("observed", self.observed),
                reg("last_cycle", self.last_cycle),
                reg("catch_up_cycle", self.catch_up_cycle),
                reg("reset_hz", self.reset_hz),
                reg("reset_count", self.reset_count),
                reg("accesses", self.accesses),
            ],
            internal_ram: None,
            program_counter: None,
            cycle_count: self.counter,
        }
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Pull a named register value out of a chip's debug snapshot.
fn reg(state: &ExpansionChipState, name: &str) -> u32 {
    state
        .registers
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("register {name} not found"))
        .value
}

/// Debug snapshot for one chip id.
fn debug_of(set: &ChipSet, id: u8) -> ExpansionChipState {
    set.debug_states()
        .into_iter()
        .find(|(i, _)| *i == id)
        .map(|(_, s)| s)
        .unwrap_or_else(|| panic!("no chip with id {id}"))
}

// ---------------------------------------------------------------------------
// Construction / bookkeeping
// ---------------------------------------------------------------------------

#[test]
fn default_chipset_is_empty() {
    let set = ChipSet::default();
    assert!(set.is_empty());
    assert_eq!(set.len(), 0);
    assert!(!set.has_observers());
}

#[test]
fn push_assigns_sequential_ids() {
    let mut set = ChipSet::default();
    let id0 = set.push(FakeChip::boxed(ExpansionChipType::DSP1, "DSP-1"));
    let id1 = set.push(FakeChip::boxed(ExpansionChipType::SDD1, "S-DD1"));
    assert_eq!(id0, 0);
    assert_eq!(id1, 1);
    assert_eq!(set.len(), 2);
    assert!(!set.is_empty());
}

#[test]
fn insert_places_chip_at_explicit_id_leaving_holes() {
    let mut set = ChipSet::default();
    // Place a single chip at id 1; id 0 stays an empty slot. This is the
    // DSP-1-at-id-1 shape a console's map plan carries with no chip at id 0.
    set.insert(1, FakeChip::boxed(ExpansionChipType::DSP1, "at-1"));

    // Only the occupied id is reachable; the hole reads as absent.
    assert!(set.get(0).is_none());
    assert!(set.get(1).is_some());
    assert_eq!(set.read(0, 0, 0), None);
    // The primary-chip picker skips the hole at id 0.
    assert_eq!(set.first_id(), Some(1));

    // Dispatch routes to id 1.
    set.write(1, 0, 0x2A, 500);
    assert_eq!(set.read(1, 0, 600), Some(0x2A));

    // len / serialize / debug_states / labels enumerate the present chip only.
    assert_eq!(set.len(), 1);
    assert!(!set.is_empty());
    assert_eq!(
        set.serialize()
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(
        set.debug_states()
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(set.labels(), vec![(1, "at-1".to_string())]);
}

#[test]
fn insert_overwrites_existing_id() {
    let mut set = ChipSet::default();
    set.insert(0, FakeChip::boxed(ExpansionChipType::DSP1, "first"));
    set.insert(0, FakeChip::boxed(ExpansionChipType::SDD1, "second"));
    assert_eq!(set.len(), 1);
    assert_eq!(set.labels(), vec![(0, "second".to_string())]);
    assert_eq!(debug_of(&set, 0).chip_type, ExpansionChipType::SDD1);
}

// ---------------------------------------------------------------------------
// Dispatch by chip_id
// ---------------------------------------------------------------------------

#[test]
fn read_write_dispatch_by_id() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::DSP1, "a"));
    set.push(FakeChip::boxed(ExpansionChipType::SDD1, "b"));

    // Writes target distinct chips.
    set.write(0, 0x00, 0x10, 100);
    set.write(1, 0x00, 0x20, 200);

    // The write cycle was recorded on the right chip (checked before any read
    // overwrites `last_cycle`).
    assert_eq!(reg(&debug_of(&set, 0), "last_cycle"), 100);
    assert_eq!(reg(&debug_of(&set, 1), "last_cycle"), 200);

    // read() returns `counter + offset`; with offset 0 that is the raw counter.
    assert_eq!(set.read(0, 0, 300), Some(0x10));
    assert_eq!(set.read(1, 0, 300), Some(0x20));

    // Offset is passed through to the chip.
    assert_eq!(set.read(0, 0x05, 300), Some(0x15));
}

#[test]
fn out_of_range_id_reads_none_and_writes_noop() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::DSP1, "only"));

    assert_eq!(set.read(9, 0, 0), None);
    // Must not panic and must not affect the existing chip.
    set.write(9, 0, 0xFF, 0);
    assert_eq!(set.read(0, 0, 0), Some(0));
}

#[test]
fn accesses_counts_reads_and_writes_only() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::DSP1, "a"));

    set.write(0, 0, 1, 0);
    set.read(0, 0, 0);
    set.read(0, 0, 0);
    set.observe_write(0, 0); // no observers registered -> reaches nobody
    set.catch_up_all(0);
    // 1 write + 2 reads = 3; observe/catch_up are not MMIO accesses.
    assert_eq!(reg(&debug_of(&set, 0), "accesses"), 3);
}

// ---------------------------------------------------------------------------
// Passive bus observation
// ---------------------------------------------------------------------------

#[test]
fn observe_write_fans_out_by_range() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::SDD1, "wide")); // id 0
    set.push(FakeChip::boxed(ExpansionChipType::SA1, "narrow")); // id 1
    set.push(FakeChip::boxed(ExpansionChipType::CX4, "elsewhere")); // id 2

    set.add_observer(0x4300..=0x437F, 0); // whole DMA reg block
    set.add_observer(0x4300..=0x4305, 1); // overlapping subset
    set.add_observer(0x2100..=0x21FF, 2); // disjoint

    assert!(set.has_observers());

    // Inside both 0 and 1's ranges.
    set.observe_write(0x4302, 0x11);
    // Inside 0 only (0x4370 > 0x4305).
    set.observe_write(0x4370, 0x22);
    // Inside nobody's range.
    set.observe_write(0x0000, 0x44);

    assert_eq!(reg(&debug_of(&set, 0), "observed"), 0x11 + 0x22);
    assert_eq!(reg(&debug_of(&set, 1), "observed"), 0x11);
    assert_eq!(reg(&debug_of(&set, 2), "observed"), 0);

    // Observation is not an MMIO access.
    assert_eq!(reg(&debug_of(&set, 0), "accesses"), 0);
}

#[test]
fn observe_write_to_missing_chip_is_ignored() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::SDD1, "a"));
    // Observer points at a chip_id that does not exist.
    set.add_observer(0x4300..=0x437F, 7);
    // Must not panic.
    set.observe_write(0x4310, 0x55);
    assert_eq!(reg(&debug_of(&set, 0), "observed"), 0);
}

// ---------------------------------------------------------------------------
// catch_up / reset fan-out
// ---------------------------------------------------------------------------

#[test]
fn catch_up_all_reaches_every_chip() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::DSP1, "a"));
    set.push(FakeChip::boxed(ExpansionChipType::SDD1, "b"));

    set.catch_up_all(123_456);

    assert_eq!(reg(&debug_of(&set, 0), "catch_up_cycle"), 123_456);
    assert_eq!(reg(&debug_of(&set, 1), "catch_up_cycle"), 123_456);
}

#[test]
fn reset_all_resets_every_chip_and_reanchors() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::DSP1, "a"));
    set.push(FakeChip::boxed(ExpansionChipType::SDD1, "b"));

    set.write(0, 0, 0x30, 0);
    set.write(1, 0, 0x40, 0);
    set.reset_all(HZ);

    for id in 0..2 {
        let s = debug_of(&set, id);
        assert_eq!(reg(&s, "counter"), 0, "chip {id} state cleared");
        assert_eq!(reg(&s, "reset_count"), 1);
        assert_eq!(u64::from(reg(&s, "reset_hz")), HZ & 0xFFFF_FFFF);
    }
}

// ---------------------------------------------------------------------------
// Savestate round-trip
// ---------------------------------------------------------------------------

#[test]
fn serialize_restore_round_trip() {
    let mut a = ChipSet::default();
    a.push(FakeChip::boxed(ExpansionChipType::DSP1, "a"));
    a.push(FakeChip::boxed(ExpansionChipType::SDD1, "b"));

    // Drive some state into both chips.
    a.write(0, 0, 0x12, 10);
    a.write(0, 0, 0x03, 20); // counter = 0x15
    a.add_observer(0x4300..=0x437F, 1);
    a.observe_write(0x4310, 0x07); // chip 1 observed = 0x07
    a.catch_up_all(999);

    let states = a.serialize();
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].0, 0);
    assert_eq!(states[1].0, 1);

    // Fresh set with fresh chips, restored from A's blobs.
    let mut b = ChipSet::default();
    b.push(FakeChip::boxed(ExpansionChipType::DSP1, "a"));
    b.push(FakeChip::boxed(ExpansionChipType::SDD1, "b"));
    b.restore(&states, HZ);

    // Opaque bytes reproduce identically.
    assert_eq!(a.serialize(), b.serialize());

    // And the observable state matches field-by-field.
    for id in 0..2 {
        let sa = debug_of(&a, id);
        let sb = debug_of(&b, id);
        assert_eq!(reg(&sa, "counter"), reg(&sb, "counter"));
        assert_eq!(reg(&sa, "observed"), reg(&sb, "observed"));
        assert_eq!(reg(&sa, "catch_up_cycle"), reg(&sb, "catch_up_cycle"));
    }
    // Restore re-anchored the clock.
    assert_eq!(
        u64::from(reg(&debug_of(&b, 0), "reset_hz")),
        HZ & 0xFFFF_FFFF
    );
}

#[test]
fn restore_drops_incompatible_blob_and_unknown_id() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::DSP1, "a"));
    set.write(0, 0, 0x55, 0); // counter = 0x55

    // A too-short blob for id 0 and a blob for a nonexistent id 5.
    set.restore(&[(0, vec![1, 2, 3]), (5, vec![0u8; 32])], HZ);

    // Chip 0's state is untouched (blob dropped), clock still re-anchored.
    assert_eq!(reg(&debug_of(&set, 0), "counter"), 0x55);
    assert_eq!(
        u64::from(reg(&debug_of(&set, 0), "reset_hz")),
        HZ & 0xFFFF_FFFF
    );
}

// ---------------------------------------------------------------------------
// Debug + labels enumeration
// ---------------------------------------------------------------------------

#[test]
fn debug_states_and_labels_enumerate_all_chips() {
    let mut set = ChipSet::default();
    set.push(FakeChip::boxed(ExpansionChipType::DSP1, "DSP-1B"));
    set.push(FakeChip::boxed(ExpansionChipType::SDD1, "S-DD1"));

    let states = set.debug_states();
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].0, 0);
    assert_eq!(states[0].1.chip_type, ExpansionChipType::DSP1);
    assert_eq!(states[1].0, 1);
    assert_eq!(states[1].1.chip_type, ExpansionChipType::SDD1);

    let labels = set.labels();
    assert_eq!(
        labels,
        vec![(0, "DSP-1B".to_string()), (1, "S-DD1".to_string())]
    );
}

// ---------------------------------------------------------------------------
// ChipFactory seam
// ---------------------------------------------------------------------------

struct FakeFactory;

impl ChipFactory for FakeFactory {
    fn build(
        &self,
        kind: &str,
        _chip_id: u8,
        _rom: Arc<[u8]>,
        _master_clock_hz: u64,
    ) -> Option<Box<dyn Coprocessor>> {
        match kind {
            "fake" => Some(FakeChip::boxed(ExpansionChipType::Custom(0), "fake")),
            _ => None,
        }
    }
}

#[test]
fn chip_factory_builds_known_kind_only() {
    let factory = FakeFactory;
    let rom: Arc<[u8]> = Arc::from(vec![0u8; 16].into_boxed_slice());

    let built = factory.build("fake", 0, rom.clone(), HZ);
    assert!(built.is_some());
    assert_eq!(built.unwrap().label(), "fake");

    assert!(factory.build("unknown", 0, rom, HZ).is_none());
}

#[test]
fn factory_built_chip_installs_into_chipset() {
    let factory = FakeFactory;
    let rom: Arc<[u8]> = Arc::from(vec![0u8; 16].into_boxed_slice());

    let mut set = ChipSet::default();
    let chip = factory.build("fake", 0, rom, HZ).expect("known kind");
    let id = set.push(chip);

    set.write(id, 0, 0x09, 0);
    assert_eq!(set.read(id, 0, 0), Some(0x09));
}
