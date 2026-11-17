//! The composable cartridge's rules, which every console's cartridge keeps:
//! resolution through the plan, the four targets read, written, peeked and
//! sourced, the missing byte and the open bus, the board's hooks and their
//! order, the battery, the typed savestate bridge, the forwarders over the
//! chip set, and the builder's chip loop. Pure: a test slot, a test board
//! and test chips.

use std::sync::{Arc, Mutex};

use retro_core::cartridge::manifest::ManifestChip;
use retro_core::cartridge::*;
use serde::{Deserialize, Serialize};

const HZ: u64 = 21_477_272;

/// A slot over one linear range, as a 68000's window is.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Slot {
    start: u32,
    end: u32,
    mask: Option<u32>,
    target: MapTarget,
}

impl SlotShape for Slot {
    type Target = MapTarget;
    fn contains(&self, addr: u32) -> bool {
        (self.start..=self.end).contains(&addr)
    }
    fn linear_offset(&self, addr: u32) -> u32 {
        addr - self.start
    }
    fn mask(&self) -> Option<u32> {
        self.mask
    }
    fn target(&self) -> MapTarget {
        self.target
    }
    fn bounds(&self) -> (u32, u32) {
        (self.start, self.end)
    }
    fn size(&self) -> usize {
        (self.end - self.start + 1) as usize
    }
}

fn slot(start: u32, end: u32, target: MapTarget) -> Slot {
    Slot {
        start,
        end,
        mask: None,
        target,
    }
}

/// What a test chip saves, as a console's typed chip-state enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    Counter(u32),
    Flag(bool),
}

type Log = Arc<Mutex<Vec<String>>>;

/// A board that answers `0xEE` where nothing is, gates its RAM's writes,
/// watches `$F0`, answers a register at `$F1`, and logs each hook.
struct TestBoard {
    log: Log,
    writable: bool,
    watched: u8,
}

impl Board for TestBoard {
    type Slot = Slot;
    type Chip = dyn Coprocessor;
    type ChipState = State;
    const MISSING: Option<u8> = Some(0xEE);

    fn sram_writable(&self) -> bool {
        self.writable
    }
    fn sram_source_place(&self, rom_len: usize, offset: u32) -> SourcePlace {
        if self.writable {
            SourcePlace::CartridgeRam
        } else {
            SourcePlace::Rom(offset % rom_len as u32)
        }
    }
    fn observe_write(&mut self, addr: u32, value: u8) {
        if addr == 0xF0 {
            self.watched = value;
            self.log.lock().unwrap().push("board".into());
        }
    }
    fn observes_bus(&self) -> bool {
        true
    }
    fn read_register(&self, addr: u32) -> Option<u8> {
        (addr == 0xF1).then_some(self.watched)
    }
    fn after_chip_write(&mut self, _chips: &ChipSetOf<dyn Coprocessor>) {
        self.log.lock().unwrap().push("after".into());
    }
    fn reset(&mut self) {
        self.watched = 0;
    }
}

/// A board with no hook of its own, whose missing byte is the open bus.
struct PlainBoard;

impl Board for PlainBoard {
    type Slot = Slot;
    type Chip = dyn Coprocessor;
    type ChipState = State;
    const MISSING: Option<u8> = None;
}

/// A chip that saves a [`State`] (or the bytes `blob` holds), logs what it
/// observes, and holds a battery when asked.
struct Chip {
    info: ExpansionChipInfo,
    label: &'static str,
    state: State,
    blob: Option<Vec<u8>>,
    log: Log,
    battery: Option<Vec<u8>>,
    battery_dirty: bool,
    accesses: u64,
    reset_hz: u64,
    irq: bool,
}

impl Chip {
    fn new(label: &'static str, state: State, log: &Log) -> Self {
        Self {
            info: ExpansionChipInfo {
                name: label.into(),
                chip_type: ExpansionChipType::Custom(9),
                description: String::new(),
                features: Vec::new(),
            },
            label,
            state,
            blob: None,
            log: Arc::clone(log),
            battery: None,
            battery_dirty: false,
            accesses: 0,
            reset_hz: 0,
            irq: false,
        }
    }
    fn with_battery(mut self, bytes: Vec<u8>) -> Self {
        self.battery = Some(bytes);
        self
    }
    fn with_blob(mut self, blob: Vec<u8>) -> Self {
        self.blob = Some(blob);
        self
    }
}

impl Coprocessor for Chip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }
    fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
        self.accesses += 1;
        offset as u8 ^ 0x80
    }
    fn write(&mut self, _offset: u32, value: u8, _master_cycle: u64) {
        self.accesses += 1;
        self.state = State::Counter(u32::from(value));
        self.log.lock().unwrap().push("write".into());
    }
    fn peek(&self, offset: u32) -> Option<u8> {
        (offset < 0x10).then_some(offset as u8 ^ 0x80)
    }
    fn source_place(&self, offset: u32) -> SourcePlace {
        SourcePlace::Rom(offset + 0x1000)
    }
    fn observe_write(&mut self, _addr: u32, _value: u8) {
        self.log.lock().unwrap().push("chip".into());
    }
    fn reset(&mut self, master_clock_hz: u64) {
        self.reset_hz = master_clock_hz;
    }
    fn has_irq(&self) -> bool {
        self.irq
    }
    fn serialize(&self) -> Vec<u8> {
        self.blob
            .clone()
            .unwrap_or_else(|| bincode::serialize(&self.state).unwrap())
    }
    fn restore(&mut self, data: &[u8], master_clock_hz: u64) {
        self.reset_hz = master_clock_hz;
        if let Ok(state) = bincode::deserialize(data) {
            self.state = state;
        }
    }
    fn debug_state(&self) -> ExpansionChipState {
        ExpansionChipState {
            chip_type: self.info.chip_type,
            registers: Vec::new(),
            internal_ram: None,
            program_counter: None,
            cycle_count: self.reset_hz,
        }
    }
    fn label(&self) -> &str {
        self.label
    }
    fn accesses(&self) -> u64 {
        self.accesses
    }
    fn battery(&self) -> Option<&[u8]> {
        self.battery.as_deref()
    }
    fn restore_battery(&mut self, data: &[u8]) {
        if let Some(battery) = self.battery.as_mut() {
            let n = data.len().min(battery.len());
            battery[..n].copy_from_slice(&data[..n]);
            self.battery_dirty = true;
        }
    }
    fn battery_dirty(&self) -> bool {
        self.battery_dirty
    }
    fn clear_battery_dirty(&mut self) {
        self.battery_dirty = false;
    }
}

/// ROM bytes `0, 1, 2, ...` of `len`.
fn rom(len: usize) -> Arc<[u8]> {
    (0..len).map(|i| i as u8).collect::<Vec<u8>>().into()
}

/// A plan of every target: a ROM window mirrored by its mask, a RAM window,
/// a chip window at chip 2 with base 0x100, an open-bus window, and a ROM
/// window ahead of a broader one it is carved out of.
fn plan() -> MapPlan<Slot> {
    let mut plan = MapPlan::new("test");
    plan.push(slot(0x4000, 0x40FF, MapTarget::Rom { base_offset: 0x20 }));
    plan.push(Slot {
        mask: Some(0xFF),
        ..slot(0x0000, 0x3FFF, MapTarget::Rom { base_offset: 0 })
    });
    plan.push(slot(0x8000, 0x80FF, MapTarget::Sram { base_offset: 0 }));
    plan.push(slot(
        0x9000,
        0x90FF,
        MapTarget::ChipMmio {
            chip_id: 2,
            base: 0x100,
        },
    ));
    plan.push(slot(0xA000, 0xA0FF, MapTarget::OpenBus));
    plan.push(slot(
        0xB000,
        0xB0FF,
        MapTarget::ChipMmio {
            chip_id: 5,
            base: 0,
        },
    ));
    plan
}

fn cartridge(log: &Log, writable: bool) -> ComposableCartridge<TestBoard> {
    let mut chips = ChipSet::default();
    chips.insert(2, Box::new(Chip::new("two", State::Counter(2), log)));
    chips.add_observer(0xF0..=0xF0, 2);
    ComposableCartridge::new(
        rom(0x100),
        plan(),
        chips,
        CartridgeInfo::new(ConsoleSystem::Genesis, &[1, 2, 3]),
        TestBoard {
            log: Arc::clone(log),
            writable,
            watched: 0,
        },
    )
    .with_sram(vec![0x55; 0x40])
}

#[test]
fn an_address_resolves_to_the_first_slot_that_covers_it_and_its_mask() {
    let mut cart = cartridge(&Log::default(), true);
    assert_eq!(
        cart.read_bus(0x4005, 0, 0xFF),
        0x25,
        "the carved-out window"
    );
    assert_eq!(cart.read_bus(0x0123, 0, 0xFF), 0x23, "the mask folds 0x123");
    assert_eq!(cart.read_bus(0x3FFF, 0, 0xFF), 0xFF);
}

#[test]
fn each_target_reads_writes_peeks_and_names_its_source() {
    let log = Log::default();
    let mut cart = cartridge(&log, true);

    // ROM past its image reads the missing byte, open bus the caller's value,
    // no slot the caller's value.
    assert_eq!(cart.read_bus(0x40F0, 0, 0x11), 0xEE);
    assert_eq!(cart.read_bus(0xA010, 0, 0x11), 0x11);
    assert_eq!(cart.read_bus(0xC000, 0, 0x11), 0x11);
    assert_eq!(cart.peek_bus(0x40F0, 0x11), 0xEE);
    assert_eq!(cart.peek_bus(0xC000, 0x11), 0x11);

    // RAM: read, written, past its size the missing byte.
    assert_eq!(cart.read_bus(0x8001, 0, 0x11), 0x55);
    cart.write_bus(0x8001, 0x99, 0);
    assert_eq!(cart.read_bus(0x8001, 0, 0x11), 0x99);
    assert_eq!(cart.peek_bus(0x8001, 0x11), 0x99);
    assert_eq!(cart.read_bus(0x8050, 0, 0x11), 0xEE);

    // A chip's window: its base added; a peek where it cannot is the open
    // bus; a chip id with no chip reads the missing byte and peeks open bus.
    assert_eq!(cart.read_bus(0x9003, 0, 0x11), 0x83, "the chip reads 0x103");
    assert_eq!(
        cart.peek_bus(0x9003, 0x11),
        0x11,
        "the chip peeks below 0x10 only"
    );
    assert_eq!(cart.read_bus(0xB003, 0, 0x11), 0xEE);
    assert_eq!(cart.peek_bus(0xB003, 0x11), 0x11);

    // ROM and open-bus writes are dropped.
    cart.write_bus(0x4000, 0x77, 0);
    cart.write_bus(0xA000, 0x77, 0);
    assert_eq!(cart.read_bus(0x4000, 0, 0xFF), 0x20);

    // Where each byte comes from.
    assert_eq!(cart.source_place(0x4005), SourcePlace::Rom(0x25));
    assert_eq!(
        cart.source_place(0x40F0),
        SourcePlace::Unknown,
        "past the image"
    );
    assert_eq!(cart.source_place(0x8001), SourcePlace::CartridgeRam);
    assert_eq!(cart.source_place(0x9003), SourcePlace::Rom(0x1103));
    assert_eq!(
        cart.source_place(0xB003),
        SourcePlace::Coprocessor,
        "no chip"
    );
    assert_eq!(cart.source_place(0xA003), SourcePlace::Unknown);
    assert_eq!(cart.source_place(0xC000), SourcePlace::Unknown);
}

#[test]
fn the_missing_byte_is_the_open_bus_on_a_board_that_names_none() {
    let mut cart = ComposableCartridge::new(
        rom(0x10),
        plan(),
        ChipSet::default(),
        CartridgeInfo::new(ConsoleSystem::NES, &[]),
        PlainBoard,
    );
    assert_eq!(cart.read_bus(0x40F0, 0, 0x3C), 0x3C);
    assert_eq!(cart.read_bus(0x8000, 0, 0x3C), 0x3C, "no RAM");
    assert_eq!(cart.read_bus(0x9000, 0, 0x3C), 0x3C, "no chip");
    assert!(!cart.observes_bus(), "nothing observes");
    assert_eq!(cart.read_register(0xF1), None);
}

#[test]
fn the_board_gates_the_ram_and_names_its_source() {
    let mut cart = cartridge(&Log::default(), false);
    cart.write_bus(0x8001, 0x99, 0);
    assert_eq!(
        cart.read_bus(0x8001, 0, 0x11),
        0x55,
        "the write was refused"
    );
    assert!(!cart.is_sram_dirty());
    assert_eq!(cart.source_place(0x8003), SourcePlace::Rom(3));
}

#[test]
fn the_board_observes_first_then_the_chips_then_hears_of_the_write() {
    let log = Log::default();
    let mut cart = cartridge(&log, true);
    assert!(cart.observes_bus());
    cart.write_bus(0xF0, 0x42, 0);
    assert_eq!(*log.lock().unwrap(), ["board", "chip", "after"]);
    assert_eq!(cart.read_register(0xF1), Some(0x42));
    assert_eq!(cart.read_register(0xF2), None);

    log.lock().unwrap().clear();
    cart.write_bus(0x9001, 0x07, 0);
    assert_eq!(*log.lock().unwrap(), ["write", "after"]);

    cart.reset();
    assert_eq!(cart.read_register(0xF1), Some(0), "the board's reset");
    cart.board_mut().watched = 5;
    assert_eq!(cart.board().watched, 5);
    assert_eq!(cart.rom_arc().len(), 0x100);
}

#[test]
fn the_save_is_the_stock_ram_unless_a_chip_with_a_battery_is_named() {
    let log = Log::default();
    let mut cart = cartridge(&log, true);
    assert_eq!(cart.sram(), &[0x55; 0x40][..]);
    cart.write_bus(0x8000, 1, 0);
    assert!(cart.is_sram_dirty());
    cart.clear_sram_dirty();
    assert!(!cart.is_sram_dirty());
    cart.restore_sram(&[9, 9]);
    assert_eq!(&cart.sram()[..3], &[9, 9, 0x55], "the prefix both hold");
    cart.write_bus(0x8000, 1, 0);
    cart.reset();
    assert!(!cart.is_sram_dirty(), "a reset clears the flag");

    // A chip named with no battery leaves the stock RAM the save.
    let named = cartridge(&log, true).with_battery_chip(2);
    assert_eq!(named.sram().len(), 0x40);

    let mut chips = ChipSet::default();
    chips.insert(
        3,
        Box::new(Chip::new("battery", State::Flag(true), &log).with_battery(vec![7; 4])),
    );
    let mut cart = ComposableCartridge::new(
        rom(0x10),
        plan(),
        chips,
        CartridgeInfo::new(ConsoleSystem::SNES, &[]),
        PlainBoard,
    )
    .with_sram(vec![0; 8])
    .with_battery_chip(3);
    assert_eq!(cart.sram(), &[7; 4][..]);
    cart.restore_sram(&[1, 2, 3, 4, 5]);
    assert_eq!(cart.sram(), &[1, 2, 3, 4][..]);
    assert!(cart.is_sram_dirty(), "the chip's own flag");
    cart.clear_sram_dirty();
    assert!(!cart.is_sram_dirty());
}

#[test]
fn the_chips_save_as_the_consoles_typed_list_and_restore_from_it() {
    let log = Log::default();
    let mut chips = ChipSet::default();
    chips.insert(1, Box::new(Chip::new("one", State::Counter(10), &log)));
    chips.insert(4, Box::new(Chip::new("four", State::Flag(true), &log)));
    chips.insert(
        6,
        Box::new(Chip::new("empty", State::Counter(0), &log).with_blob(Vec::new())),
    );
    chips.insert(
        7,
        Box::new(Chip::new("foreign", State::Counter(0), &log).with_blob(vec![0xFF; 3])),
    );
    let mut cart = ComposableCartridge::new(
        rom(0x10),
        plan(),
        chips,
        CartridgeInfo::new(ConsoleSystem::SNES, &[]),
        PlainBoard,
    );
    let saved = cart.coprocessors_savestate();
    assert_eq!(saved, [(1, State::Counter(10)), (4, State::Flag(true))]);

    cart.restore_coprocessors_savestate(&[(1, State::Counter(77)), (9, State::Flag(false))], HZ);
    assert_eq!(
        cart.coprocessors_savestate(),
        [(1, State::Counter(77)), (4, State::Flag(true))],
        "a state with no chip at its id is dropped"
    );
}

#[test]
fn the_forwarders_reach_the_chip_set_and_the_primary_chip_is_the_lowest_id() {
    let log = Log::default();
    let mut chips = ChipSet::default();
    chips.insert(5, Box::new(Chip::new("five", State::Counter(5), &log)));
    let mut irq = Chip::new("three", State::Counter(3), &log);
    irq.irq = true;
    chips.insert(3, Box::new(irq));
    let mut cart = ComposableCartridge::new(
        rom(0x10),
        plan(),
        chips,
        CartridgeInfo::new(ConsoleSystem::SNES, &[]),
        PlainBoard,
    );
    assert_eq!(cart.coprocessor_label(), Some("three"));
    assert!(cart.has_irq(), "any chip");
    assert_eq!(cart.read_chip(5, 1, 0, 0), 0x81);
    assert_eq!(
        cart.coprocessor_accesses(),
        0,
        "the primary chip read nothing"
    );
    cart.reset_chip(HZ);
    assert_eq!(cart.coprocessor_debug_state().unwrap().cycle_count, HZ);
    assert_eq!(cart.coprocessors_debug_state().len(), 2);
    assert_eq!(
        cart.coprocessor_labels(),
        [(3, "three".to_string()), (5, "five".to_string())]
    );
    assert!(cart.secondary_cpu_state().is_none());
    assert!(cart.step_secondary_cpu().is_none());
    assert!(cart.disassemble_secondary(0, 4).is_empty());
    assert!(cart.take_secondary_trace().is_empty());
    assert!(!cart.needs_tick() && !cart.produces_audio());
}

#[test]
fn a_cartridge_with_no_chip_has_no_coprocessor_to_report() {
    let cart = ComposableCartridge::new(
        rom(0x10),
        plan(),
        ChipSet::default(),
        CartridgeInfo::new(ConsoleSystem::SNES, &[]),
        PlainBoard,
    );
    assert_eq!(cart.coprocessor_label(), None);
    assert_eq!(cart.coprocessor_accesses(), 0);
    assert!(cart.coprocessor_debug_state().is_none());
    assert!(cart.coprocessors_debug_state().is_empty());
    assert!(cart.coprocessor_labels().is_empty());
    assert!(cart.coprocessors_savestate().is_empty());
    assert!(!cart.has_irq());
}

/// A factory that knows one kind of chip.
struct Factory(Log);

impl ChipFactory for Factory {
    fn build(
        &self,
        kind: &str,
        _chip_id: u8,
        _rom: Arc<[u8]>,
        _master_clock_hz: u64,
    ) -> Option<Box<dyn Coprocessor>> {
        (kind == "known").then(|| Box::new(Chip::new("known", State::Counter(1), &self.0)) as _)
    }
}

#[test]
fn the_chip_loop_installs_each_declared_chip_with_its_observers() {
    let log = Log::default();
    let rom = rom(0x10);
    let mut chips = ChipSet::default();
    install_chips(
        &Factory(Arc::clone(&log)),
        [
            ChipDecl {
                id: 4,
                kind: "known".into(),
                observes: vec![0x10..=0x1F, 0x40..=0x40],
            },
            ChipDecl {
                id: 1,
                kind: "known".into(),
                observes: Vec::new(),
            },
        ],
        &rom,
        HZ,
        &mut chips,
    )
    .unwrap();
    assert_eq!(chips.len(), 2);
    assert_eq!(chips.first_id(), Some(1));
    chips.observe_write(0x15, 1);
    assert_eq!(*log.lock().unwrap(), ["chip"]);
    chips.observe_write(0x30, 1);
    chips.observe_write(0x40, 1);
    assert_eq!(
        *log.lock().unwrap(),
        ["chip", "chip"],
        "each range observed, nothing between"
    );

    let unknown = install_chips(
        &Factory(Arc::clone(&log)),
        [ChipDecl {
            id: 0,
            kind: "zzz".into(),
            observes: Vec::new(),
        }],
        &rom,
        HZ,
        &mut chips,
    )
    .unwrap_err();
    assert_eq!(unknown.to_string(), "unknown coprocessor chip kind: zzz");
}

#[test]
fn a_manifest_chip_becomes_a_declaration_named_by_its_path() {
    let chip = ManifestChip {
        id: 3,
        kind: "sdd1".into(),
        mapping: (),
        observes: vec!["0x4300-0x437F".into(), " 10 - 1F ".into()],
    };
    let decl = ChipDecl::from_manifest(&chip, 2).unwrap();
    assert_eq!(
        decl,
        ChipDecl {
            id: 3,
            kind: "sdd1".into(),
            observes: vec![0x4300..=0x437F, 0x10..=0x1F],
        }
    );
    let bad = ManifestChip {
        observes: vec!["0x1-0x2".into(), "0x2-0x1".into()],
        ..chip
    };
    assert_eq!(
        ChipDecl::from_manifest(&bad, 2).unwrap_err().to_string(),
        "invalid hex in field 'chips[2].observes[1]': 0x2-0x1"
    );
}

#[test]
fn a_cartridge_info_from_its_image_holds_its_size_and_hash() {
    let info = CartridgeInfo::new(ConsoleSystem::SNES, b"abc");
    assert_eq!(info.rom_size, 3);
    assert_eq!(
        info.rom_hash,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(info.rom_hash, rom_sha256(b"abc"));
    assert_eq!(
        rom_sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert!(info.title.is_empty() && info.mapper_info.is_none());
    assert_eq!(info.detection_source, DetectionSource::Header);
}
