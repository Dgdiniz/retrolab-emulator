//! DSP-1 coprocessor cartridge: the register window and its backends.
//!
//! Wraps a [`PlainCartridge`] (the ROM + battery SRAM) with the DSP-1's
//! DR/SR register window. Behind it runs either the firmware-free HLE
//! or, with the `nec-lle` feature, an emulated NEC uPD7725 (the
//! `upd7725` crate) running a real DSP-1 firmware dump. The
//! cartridge owns its combined [`MapPlan`], the stock ROM/SRAM slots
//! *plus* the DSP register window, so the bus dispatches ROM reads to
//! the inner cartridge and the DSP register window here, via
//! [`SnesCartridge::read_chip`] / [`SnesCartridge::write_chip`].
//!
//! **Timing.** The uPD7725 runs at 8 MHz, asynchronous to the 21.477 MHz
//! SNES master clock. Rather than co-scheduling, the chip is driven by
//! *lazy catch-up* (the same model as the APU): every register access,
//! and once per frame at the `FrameEnd` hook, the core is stepped
//! forward to the master cycle of that access via an
//! [`IntegerRateConverter`]. Because the only DSP state the CPU can
//! observe is `DR`/`SR`, catching up exactly at each access is
//! observationally identical to running the two clocks in lockstep, but
//! far cheaper.
//!
//! **DR vs SR.** The DSP exposes exactly two host registers: the data
//! register `DR` and the status register `SR`. Which one a given access
//! hits is selected by one address bit, and *which* bit depends on the
//! cartridge layout ([`Dsp1Layout`]):
//!
//! - LoROM DSP-1 (`$20-$3F`/`$A0-$BF:8000-FFFF`, e.g. Pilotwings):
//!   **bit 14** (`$8000-$BFFF` = DR, `$C000-$FFFF` = SR).
//! - HiROM DSP-1 (`$00-$1F`/`$80-$9F:6000-7FFF`, e.g. Super Mario
//!   Kart): **bit 12** (`$6000-$6FFF` = DR, `$7000-$7FFF` = SR).
//!
//! Each register slot is built with `.with_mask(sr_select_bit)`, so the
//! offset the bus hands `read_chip`/`write_chip` is already reduced to
//! exactly `0` (DR) or `sr_select_bit` (SR), the chip decodes with a
//! single AND against its stored `sr_select_bit`.
//!
//! **Backend selection.** The DSP-1 ships as a [`Dsp1Chip`] implementing
//! the generic [`Coprocessor`] trait, placed at [`DSP1_CHIP_ID`] in a
//! [`ComposableCartridge`]'s chip set. Three backends sit behind the same
//! register window:
//!
//! - **LLE**: a real uPD7725 (the `upd7725` crate, built only with the
//!   `nec-lle` feature) running the firmware dump, lazily clocked (above).
//! - **HLE**: a command-level reimplementation
//!   ([`Dsp1Hle`](super::dsp1_hle::Dsp1Hle)) that needs no firmware dump.
//! - **Idle stub**: a defensive last resort when LLE was asked for and
//!   cannot run (no firmware, or a build without the `nec-lle` feature):
//!   `SR` always reports ready and `DR` reads back `0`, so a status-poll
//!   loop never hard-hangs (the game still misbehaves).

use std::sync::Arc;

#[cfg(feature = "nec-lle")]
use retro_core::audio::IntegerRateConverter;
#[cfg(feature = "nec-lle")]
use retro_core::cartridge::ChipRegister;
use retro_core::cartridge::{
    ChipSet, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
};
#[cfg(feature = "nec-lle")]
use retro_core::cpu::Resettable;
#[cfg(feature = "nec-lle")]
use upd7725::Upd7725;

use crate::savestate::CoprocessorState;
#[cfg(feature = "nec-lle")]
use crate::savestate::Dsp1State;

#[cfg(feature = "nec-lle")]
use super::firmware::{ByteOrder, Dsp1Firmware, Dsp1Revision};
use super::header::SnesHeader;
use super::{BusSpeed, ComposableCartridge, MapSlot, MapTarget, PlainCartridge};

/// `chip_id` the bus dispatch carries for the DSP-1. Matches the
/// manifest convention (`chips[].id = 1`). A second NEC chip (ST010, ...)
/// would claim a distinct id.
pub const DSP1_CHIP_ID: u8 = 1;

/// uPD7725 clock inside the DSP-1 package.
#[cfg(feature = "nec-lle")]
pub const DSP1_CLOCK_HZ: u64 = 8_000_000;

/// NTSC master clock, the converter's default denominator at build
/// time. `reset_chip` rebinds it to the console's actual region clock
/// (mirroring how the APU re-anchors in `SnesEmulator::reset`).
pub const DEFAULT_MASTER_CLOCK_HZ: u64 = 21_477_270;

/// Where a DSP-1 cartridge exposes its register window: one of the three
/// boards the chip shipped on (checked against bsnes-plus's mapping).
///
/// The variant fixes both the bank/offset windows the chip answers at
/// *and* the address bit that selects `SR` vs `DR` ([`Self::sr_select_bit`]).
/// Detection ([`detect_dsp1_layout`]) keys on the header's map-mode and
/// chipset bytes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dsp1Layout {
    /// LoROM, ROM <= 1 MiB: `$20-$3F` & `$A0-$BF : $8000-$FFFF`.
    /// DR/SR by bit 14 (`$8000-$BFFF` = DR, `$C000-$FFFF` = SR).
    /// Pilotwings and most LoROM DSP-1 titles.
    LoRom1Mb,
    /// LoROM, ROM > 1 MiB: `$60-$6F` & `$E0-$EF : $0000-$7FFF`.
    /// DR/SR by bit 14 (`$0000-$3FFF` = DR, `$4000-$7FFF` = SR).
    LoRom2Mb,
    /// HiROM: `$00-$1F` & `$80-$9F : $6000-$7FFF`. DR/SR by bit 12
    /// (`$6000-$6FFF` = DR, `$7000-$7FFF` = SR). Super Mario Kart.
    HiRom,
}

impl Dsp1Layout {
    /// Address bit (within the register window, after the slot mask)
    /// that selects `SR` (set) vs `DR` (clear): `0x4000` on the LoROM
    /// boards, `0x1000` on the HiROM one.
    pub fn sr_select_bit(self) -> u32 {
        match self {
            Dsp1Layout::LoRom1Mb | Dsp1Layout::LoRom2Mb => 0x4000,
            Dsp1Layout::HiRom => 0x1000,
        }
    }

    /// The two mirror windows the DSP answers at, as [`MapSlot`]s
    /// dispatching to `chip_id`. Each slot masks the slot-relative
    /// offset down to `{0, sr_select_bit}` so the cartridge decodes
    /// SR/DR with one AND. The `$6000-$7FFF`/`$8000-$FFFF` windows are
    /// always slow-bus (8 mclk); the DSP never sees a FastROM access.
    pub fn register_slots(self, chip_id: u8) -> [MapSlot; 2] {
        let bit = self.sr_select_bit();
        let slot = |banks, offsets| {
            MapSlot::new(
                banks,
                offsets,
                MapTarget::ChipMmio { chip_id, base: 0 },
                BusSpeed::Slow,
            )
            .with_mask(bit)
        };
        match self {
            Dsp1Layout::LoRom1Mb => [
                slot(0x20..=0x3F, 0x8000..=0xFFFF),
                slot(0xA0..=0xBF, 0x8000..=0xFFFF),
            ],
            Dsp1Layout::LoRom2Mb => [
                slot(0x60..=0x6F, 0x0000..=0x7FFF),
                slot(0xE0..=0xEF, 0x0000..=0x7FFF),
            ],
            Dsp1Layout::HiRom => [
                slot(0x00..=0x1F, 0x6000..=0x7FFF),
                slot(0x80..=0x9F, 0x6000..=0x7FFF),
            ],
        }
    }
}

/// Maker code of the one fast-LoROM cartridge with battery RAM whose DSP
/// is not a DSP-1: SD Gundam GX (Bandai), which carries the DSP-3.
const DSP3_MAKER: u8 = 0xB2;

/// Detect a DSP-1 cartridge from its parsed [`SnesHeader`], returning the
/// register-window [`Dsp1Layout`] (or `None` when the header is not a
/// DSP-1 cart). `rom_len` is the post-strip ROM size in bytes, used to
/// split LoROM 1 MiB vs 2 MiB.
///
/// The four NEC DSPs share the chipset bytes "ROM + coprocessor" (`$03`)
/// and "ROM + coprocessor + RAM + battery" (`$05`); the map mode
/// (`$FFD5`) and, in one case, the maker tell them apart. Notably Super
/// Mario Kart reports map mode `$31` (fast HiROM) + chipset `$05`, so it
/// resolves to [`Dsp1Layout::HiRom`], **not** a LoROM window. Checked
/// against bsnes-plus's header rules.
pub fn detect_dsp1_layout(header: &SnesHeader, rom_len: usize) -> Option<Dsp1Layout> {
    const ONE_MEBIBYTE: usize = 0x10_0000;

    let is_dsp1 = match (header.map_mode, header.cartridge_type) {
        // Slow LoROM or slow HiROM, no RAM.
        (0x20 | 0x21, 0x03) => true,
        // Fast LoROM with battery RAM, unless it is the DSP-3 game.
        (0x30, 0x05) => header.developer_id != DSP3_MAKER,
        // Fast HiROM, with or without RAM.
        (0x31, 0x03 | 0x05) => true,
        _ => false,
    };
    if !is_dsp1 {
        return None;
    }

    // Bit 0 of the map mode is HiROM. A LoROM board puts the chip in one of
    // two places, by the size of its ROM.
    Some(if header.map_mode & 0x01 != 0 {
        Dsp1Layout::HiRom
    } else if rom_len <= ONE_MEBIBYTE {
        Dsp1Layout::LoRom1Mb
    } else {
        Dsp1Layout::LoRom2Mb
    })
}

/// How a coprocessor should be emulated when a chip cartridge is built.
///
/// [`CoprocessorMode::Auto`] uses LLE when firmware is present and
/// otherwise the firmware-free HLE; [`CoprocessorMode::Lle`] forces LLE and
/// falls back to the idle stub when no firmware is staged (or the build
/// has no LLE, see [`NEC_LLE_BUILT`]);
/// [`CoprocessorMode::Hle`] always uses the firmware-free HLE backend.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum CoprocessorMode {
    /// LLE if firmware is available, else HLE.
    #[default]
    Auto,
    /// Force low-level emulation (firmware required; idle stub if absent).
    Lle,
    /// Force high-level emulation (no firmware needed).
    Hle,
}

/// Whether this build carries the DSP-1's low-level emulation (the
/// `nec-lle` feature): the `upd7725` core that runs a firmware dump.
/// Without it a staged firmware is ignored, [`CoprocessorMode::Auto`]
/// runs the HLE and a forced [`CoprocessorMode::Lle`] installs the stub
/// [`Dsp1Chip::new_not_built`] makes.
pub const NEC_LLE_BUILT: bool = cfg!(feature = "nec-lle");

/// The active emulation backend behind the register window.
enum Backend {
    /// Real uPD7725 firmware, lazily clocked to the access master cycle.
    /// The core is boxed: its 256-word internal data RAM makes the inline
    /// variant ~700 bytes, dwarfing the empty `Idle` variant.
    #[cfg(feature = "nec-lle")]
    Lle {
        core: Box<Upd7725>,
        sync: IntegerRateConverter,
        dsp_cycles: u64,
    },
    /// No firmware, no HLE: a non-hanging placeholder (SR ready, DR reads
    /// 0). Defensive last resort.
    Idle,
}

/// The DSP-1 coprocessor as a [`Coprocessor`].
///
/// Holds *only* the coprocessor state behind the DR/SR window, the
/// ROM/SRAM live in the [`ComposableCartridge`] whose chip set hosts this
/// chip. The backend is either a firmware-running uPD7725 (LLE) or the idle
/// stub; the firmware-free HLE backend lives in
/// [`Dsp1Hle`](super::dsp1_hle::Dsp1Hle) and is selected by the builder.
pub struct Dsp1Chip {
    backend: Backend,
    /// Address bit (post-slot-mask) that selects `SR` over `DR`.
    /// Sourced from [`Dsp1Layout::sr_select_bit`] at build time.
    sr_select_bit: u32,
    /// Human-readable chip summary for the CLI `info` command, e.g.
    /// `"DSP-1B (LLE)"` or `"DSP-1 (idle stub, no firmware)"`.
    label: String,
    /// Generic chip metadata (type / name / features) for the
    /// [`Coprocessor::info`] surface. Distinct from `label`, which is the
    /// backend-specific one-liner the CLI `info` prints.
    info: ExpansionChipInfo,
    /// Count of CPU accesses (reads + writes) to the DR/SR window since
    /// reset. A non-zero count proves the running game actually drives
    /// the coprocessor (vs. merely having one detected/clocked). Surfaced
    /// for tests and the debugger via [`Self::chip_accesses`].
    accesses: u64,
}

impl Dsp1Chip {
    /// Build an LLE chip from decoded firmware. `sr_select_bit` comes from
    /// the resolved [`Dsp1Layout`].
    #[cfg(feature = "nec-lle")]
    pub fn new_lle(firmware: &Dsp1Firmware, master_clock_hz: u64, sr_select_bit: u32) -> Self {
        let core = Box::new(Upd7725::with_firmware(
            &firmware.program,
            &firmware.data_rom,
        ));
        let label = match firmware.revision {
            Some(rev) => format!("{} (LLE)", rev.name()),
            None => format!("{} (LLE, unrecognised firmware)", Dsp1Revision::Dsp1.name()),
        };
        let info = ExpansionChipInfo {
            name: label.clone(),
            chip_type: ExpansionChipType::DSP1,
            description: "NEC uPD7725 (low-level emulation)".to_string(),
            features: vec!["lle".to_string()],
        };
        Self {
            backend: Backend::Lle {
                core,
                sync: IntegerRateConverter::new(DSP1_CLOCK_HZ, master_clock_hz),
                dsp_cycles: 0,
            },
            sr_select_bit,
            label,
            info,
            accesses: 0,
        }
    }

    /// Build an idle-stub chip (no firmware, no HLE available).
    pub fn new_idle(sr_select_bit: u32) -> Self {
        Self {
            backend: Backend::Idle,
            sr_select_bit,
            label: "DSP-1 (idle stub, no firmware)".to_string(),
            info: ExpansionChipInfo {
                name: "DSP-1".to_string(),
                chip_type: ExpansionChipType::DSP1,
                description: "idle stub (no firmware, no HLE)".to_string(),
                features: Vec::new(),
            },
            accesses: 0,
        }
    }

    /// Build the idle stub a forced LLE gets from a build without the
    /// `nec-lle` feature. Its label says so, which tells this stub from
    /// the one a missing firmware leaves behind.
    pub fn new_not_built(sr_select_bit: u32) -> Self {
        Self {
            backend: Backend::Idle,
            sr_select_bit,
            label: "DSP-1 (idle stub, low-level emulation not built)".to_string(),
            info: ExpansionChipInfo {
                name: "DSP-1".to_string(),
                chip_type: ExpansionChipType::DSP1,
                description: "idle stub (low-level emulation not built into this binary)"
                    .to_string(),
                features: Vec::new(),
            },
            accesses: 0,
        }
    }

    /// `true` when this chip is running real firmware (LLE).
    pub fn is_lle(&self) -> bool {
        match self.backend {
            #[cfg(feature = "nec-lle")]
            Backend::Lle { .. } => true,
            Backend::Idle => false,
        }
    }

    /// DSP clocks executed since reset (LLE only; `0` for the idle stub).
    /// Exposed for tests and the debugger.
    pub fn dsp_cycles(&self) -> u64 {
        match &self.backend {
            #[cfg(feature = "nec-lle")]
            Backend::Lle { dsp_cycles, .. } => *dsp_cycles,
            Backend::Idle => 0,
        }
    }

    /// CPU accesses (reads + writes) to the DR/SR window since reset.
    /// Non-zero => the running game is actively driving the coprocessor.
    pub fn chip_accesses(&self) -> u64 {
        self.accesses
    }

    /// Build an [`ExpansionChipState`] snapshot of the live chip for the
    /// debugger / CLI. The LLE backend surfaces the full uPD7725 register
    /// file + data RAM (512 bytes, little-endian); the idle stub reports
    /// the chip as present with an empty register list (no firmware is
    /// driving it).
    fn debug_chip_state(&self) -> ExpansionChipState {
        match &self.backend {
            #[cfg(feature = "nec-lle")]
            Backend::Lle {
                core, dsp_cycles, ..
            } => {
                let mut addr = 0u32;
                let mut reg = |name: &str, value: u32, description: &str| {
                    let r = ChipRegister {
                        address: addr,
                        name: name.to_string(),
                        value,
                        description: description.to_string(),
                    };
                    addr += 1;
                    r
                };
                let registers = vec![
                    reg(
                        "PC",
                        core.program_counter as u32,
                        "Program counter (11-bit)",
                    ),
                    reg("RP", core.rom_pointer as u32, "Data-ROM pointer"),
                    reg("DP", core.ram_pointer as u32, "Data-RAM pointer"),
                    reg("SP", core.stack_pointer as u32, "Call-stack pointer"),
                    reg("K", core.multiplicand as u32, "Multiplier input K"),
                    reg("L", core.multiplier as u32, "Multiplier input L"),
                    reg("M", core.product_high as u32, "Multiplier output high"),
                    reg("N", core.product_low as u32, "Multiplier output low"),
                    reg("A", core.accumulators[0] as u32, "Accumulator A"),
                    reg("B", core.accumulators[1] as u32, "Accumulator B"),
                    reg("TR", core.temporary as u32, "Temporary register"),
                    reg("TRB", core.temporary_b as u32, "Temporary register B"),
                    reg("SR", core.status as u32, "Status register (host handshake)"),
                    reg("DR", core.data_port as u32, "Data register (host port)"),
                    reg("SI", core.serial_in as u32, "Serial-in shift register"),
                    reg("SO", core.serial_out as u32, "Serial-out shift register"),
                    reg("FLAGA", core.flags[0].to_bits() as u32, "ALU flags (acc A)"),
                    reg("FLAGB", core.flags[1].to_bits() as u32, "ALU flags (acc B)"),
                    reg(
                        "ACCESSES",
                        self.accesses as u32,
                        "CPU DR/SR accesses since reset (diagnostic, truncated)",
                    ),
                ];
                let mut internal_ram = Vec::with_capacity(core.data_ram.len() * 2);
                for &word in core.data_ram.iter() {
                    internal_ram.extend_from_slice(&word.to_le_bytes());
                }
                ExpansionChipState {
                    chip_type: ExpansionChipType::DSP1,
                    registers,
                    internal_ram: Some(internal_ram),
                    program_counter: Some(core.program_counter as u32),
                    cycle_count: *dsp_cycles,
                }
            }
            Backend::Idle => ExpansionChipState {
                chip_type: ExpansionChipType::DSP1,
                registers: Vec::new(),
                internal_ram: None,
                program_counter: None,
                cycle_count: 0,
            },
        }
    }

    /// Capture the chip's volatile state (LLE core + converter anchor), or
    /// `None` for the idle stub. Never includes the firmware/program ROM,
    /// that is re-staged from the host on load, like the cartridge ROM. The
    /// generic [`Coprocessor::serialize`] bridges this to opaque bytes.
    pub fn savestate(&self) -> Option<CoprocessorState> {
        match &self.backend {
            #[cfg(feature = "nec-lle")]
            Backend::Lle {
                core,
                sync,
                dsp_cycles,
            } => Some(CoprocessorState::Dsp1Lle(Dsp1State {
                core: crate::savestate::Upd7725State::capture(core),
                // Same three-field converter anchor the APU persists; the
                // num/den ratio is re-derived from the region clock on
                // restore (see `restore_savestate`).
                sync: crate::savestate::ConverterState {
                    reference_cycles: sync.reference_cycles(),
                    samples_at_reference: sync.samples_at_reference(),
                    remainder: sync.remainder(),
                },
                dsp_cycles: *dsp_cycles,
            })),
            // The idle stub has no internal state worth persisting.
            Backend::Idle => None,
        }
    }

    /// Restore volatile state, re-anchoring the converter to `master_clock_hz`.
    /// Only an LLE blob can populate the LLE backend. An HLE blob, or any blob
    /// arriving at the idle stub (firmware absent on this run), is dropped,
    /// there's no matching core to receive it.
    #[cfg(feature = "nec-lle")]
    pub fn restore_savestate(&mut self, state: &CoprocessorState, master_clock_hz: u64) {
        if let (
            CoprocessorState::Dsp1Lle(lle),
            Backend::Lle {
                core,
                sync,
                dsp_cycles,
            },
        ) = (state, &mut self.backend)
        {
            lle.core.restore(core);
            *sync = IntegerRateConverter::with_state(
                DSP1_CLOCK_HZ,
                master_clock_hz,
                lle.sync.reference_cycles,
                lle.sync.samples_at_reference,
                lle.sync.remainder,
            );
            *dsp_cycles = lle.dsp_cycles;
        }
    }

    /// Without the `nec-lle` feature no backend here holds state, so every
    /// blob is dropped.
    #[cfg(not(feature = "nec-lle"))]
    pub fn restore_savestate(&mut self, _state: &CoprocessorState, _master_clock_hz: u64) {}
}

impl Coprocessor for Dsp1Chip {
    fn info(&self) -> &ExpansionChipInfo {
        &self.info
    }

    fn read(&mut self, offset: u32, master_cycle: u64) -> u8 {
        self.catch_up(master_cycle);
        self.accesses += 1;
        let is_sr = offset & self.sr_select_bit != 0;
        match &mut self.backend {
            #[cfg(feature = "nec-lle")]
            Backend::Lle { core, .. } => {
                if is_sr {
                    core.read_status()
                } else {
                    core.read_data()
                }
            }
            // Idle stub: report "ready" on SR, zero on DR.
            Backend::Idle => {
                if is_sr {
                    0x80
                } else {
                    0x00
                }
            }
        }
    }

    fn write(&mut self, offset: u32, value: u8, master_cycle: u64) {
        self.catch_up(master_cycle);
        self.accesses += 1;
        let is_sr = offset & self.sr_select_bit != 0;
        match &mut self.backend {
            #[cfg(feature = "nec-lle")]
            Backend::Lle { core, .. } => {
                if is_sr {
                    core.write_status(value);
                } else {
                    core.write_data(value);
                }
            }
            // The idle stub drops writes.
            Backend::Idle => {
                let _ = (is_sr, value);
            }
        }
    }

    /// Step the uPD7725 forward to `master_cycle`. Converts the master
    /// cycle to a target DSP-clock count and runs that many instructions
    /// (one instruction = one DSP clock). Monotonic: a target at or
    /// behind the current anchor is a no-op. The idle stub has no clock.
    fn catch_up(&mut self, master_cycle: u64) {
        match &mut self.backend {
            #[cfg(feature = "nec-lle")]
            Backend::Lle {
                core,
                sync,
                dsp_cycles,
            } => {
                if master_cycle <= sync.reference_cycles() {
                    return;
                }
                let target = sync.cycles_to_samples(master_cycle);
                while *dsp_cycles < target {
                    core.step();
                    *dsp_cycles += 1;
                }
                sync.advance(master_cycle, target);
            }
            // The idle stub has no clock.
            Backend::Idle => {
                let _ = master_cycle;
            }
        }
    }

    fn reset(&mut self, master_clock_hz: u64) {
        match &mut self.backend {
            #[cfg(feature = "nec-lle")]
            Backend::Lle {
                core,
                sync,
                dsp_cycles,
            } => {
                core.reset();
                *sync = IntegerRateConverter::new(DSP1_CLOCK_HZ, master_clock_hz);
                *dsp_cycles = 0;
            }
            Backend::Idle => {
                let _ = master_clock_hz;
            }
        }
        self.accesses = 0;
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn accesses(&self) -> u64 {
        self.accesses
    }

    /// Volatile state as opaque bytes: a `bincode`-encoded
    /// [`CoprocessorState`] (empty for the idle stub). Bridges the console's
    /// typed savestate to the generic [`Coprocessor`] surface so the on-disk
    /// format is unchanged.
    fn serialize(&self) -> Vec<u8> {
        match self.savestate() {
            Some(state) => bincode::serialize(&state).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    fn restore(&mut self, data: &[u8], master_clock_hz: u64) {
        // A garbage/empty blob decodes to `Err` and is dropped, matching the
        // idle-stub no-op path.
        if let Ok(state) = bincode::deserialize::<CoprocessorState>(data) {
            self.restore_savestate(&state, master_clock_hz);
        }
    }

    fn debug_state(&self) -> ExpansionChipState {
        self.debug_chip_state()
    }
}

/// Assemble a single-coprocessor cartridge: place `chip` at
/// [`DSP1_CHIP_ID`] in a fresh [`ChipSet`] and re-home the plain
/// cartridge's ROM / SRAM / combined map plan behind it. The ROM moves
/// into an `Arc<[u8]>` so a future ROM-consuming chip can share the image.
fn compose_single(inner: PlainCartridge, chip: impl Coprocessor + 'static) -> ComposableCartridge {
    let (rom, sram, plan) = inner.into_parts();
    let mut chips = ChipSet::default();
    chips.insert(DSP1_CHIP_ID, Box::new(chip));
    ComposableCartridge::new(Arc::from(rom.into_boxed_slice()), sram, plan, chips)
}

/// DSP-1 entry point into the composable-cartridge machinery.
///
/// `Dsp1Cartridge` is not a cartridge type of its own, it's a thin
/// constructor namespace that pairs a [`PlainCartridge`] (ROM + battery
/// SRAM + the combined map plan, including the DSP register window) with
/// a DSP-1 backend placed at [`DSP1_CHIP_ID`] inside a
/// [`ComposableCartridge`]: a [`Dsp1Chip`] for `new_lle(...)` and
/// `new_idle(...)`, the [`Dsp1Hle`](super::dsp1_hle::Dsp1Hle) for
/// `new_hle(...)`. The constructors build a one-chip DSP-1 cartridge
/// without the caller assembling the [`ChipSet`] itself.
pub struct Dsp1Cartridge;

impl Dsp1Cartridge {
    /// Build a DSP-1 LLE cartridge. `inner` must carry the *combined* map
    /// plan (ROM + SRAM + the DSP register window); `sr_select_bit` comes
    /// from the resolved [`Dsp1Layout`].
    #[cfg(feature = "nec-lle")]
    pub fn new_lle(
        inner: PlainCartridge,
        firmware: &Dsp1Firmware,
        master_clock_hz: u64,
        sr_select_bit: u32,
    ) -> ComposableCartridge {
        compose_single(
            inner,
            Dsp1Chip::new_lle(firmware, master_clock_hz, sr_select_bit),
        )
    }

    /// Build a DSP-1 HLE cartridge (no firmware required).
    /// `sr_select_bit` comes from the resolved [`Dsp1Layout`] so the
    /// DR/SR decode matches the mapping the LLE backend would have used.
    pub fn new_hle(inner: PlainCartridge, sr_select_bit: u32) -> ComposableCartridge {
        compose_single(inner, super::dsp1_hle::Dsp1Hle::new(sr_select_bit))
    }

    /// Build a DSP-1 idle-stub cartridge (no firmware available).
    pub fn new_idle(inner: PlainCartridge, sr_select_bit: u32) -> ComposableCartridge {
        compose_single(inner, Dsp1Chip::new_idle(sr_select_bit))
    }
}

/// `true` for the chip-kind strings that denote a DSP-1 (the only NEC-DSP
/// coprocessor wired today). DSP-2/3/4 share the silicon but need their own
/// firmware + mapping, so they are not matched here. Shared by the cartridge
/// builder's header/manifest resolution and the [`ChipFactory`] impl.
///
/// [`ChipFactory`]: retro_core::cartridge::ChipFactory
pub(crate) fn is_dsp1_kind(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "dsp1" | "dsp-1" | "dsp1a" | "dsp-1a" | "dsp1b" | "dsp-1b"
    )
}

/// Choose the DSP-1 backend for a given `mode` + firmware availability and
/// return it as a boxed [`Coprocessor`], the chip only, not a cartridge:
///
/// - `Auto`: LLE when `firmware` decodes, otherwise the firmware-free HLE.
/// - `Lle`: LLE when `firmware` decodes, otherwise the idle stub (the caller
///   explicitly wanted low-level emulation, so don't silently substitute HLE;
///   surface the missing-firmware state instead).
/// - `Hle`: always the firmware-free HLE, even if firmware is staged.
///
/// A build without the `nec-lle` feature has no LLE at all: `Auto` runs the
/// HLE whatever is staged, and `Lle` installs the stub that says so.
///
/// `sr_select_bit` comes from the resolved [`Dsp1Layout`]; `master_clock_hz`
/// anchors the LLE rate converter and is re-anchored by [`Coprocessor::reset`]
/// during `SnesEmulator::reset`. This is the single backend-selection point
/// the console's [`ChipFactory`](retro_core::cartridge::ChipFactory) impl
/// uses to build the DSP-1 chip.
pub(crate) fn select_dsp1_chip(
    firmware: Option<&[u8]>,
    mode: CoprocessorMode,
    sr_select_bit: u32,
    master_clock_hz: u64,
) -> Box<dyn Coprocessor> {
    if let Some(chip) = lle_chip(firmware, mode, sr_select_bit, master_clock_hz) {
        return chip;
    }
    match mode {
        // Forced LLE that cannot run, no firmware, or a build without the
        // LLE: a stub, never a silent swap to the HLE.
        CoprocessorMode::Lle => Box::new(if NEC_LLE_BUILT {
            Dsp1Chip::new_idle(sr_select_bit)
        } else {
            Dsp1Chip::new_not_built(sr_select_bit)
        }),
        // Auto-without-firmware and forced HLE both run the firmware-free HLE.
        CoprocessorMode::Auto | CoprocessorMode::Hle => {
            Box::new(super::dsp1_hle::Dsp1Hle::new(sr_select_bit))
        }
    }
}

/// The LLE chip for `mode` and `firmware`, when this build can run it.
#[cfg(feature = "nec-lle")]
fn lle_chip(
    firmware: Option<&[u8]>,
    mode: CoprocessorMode,
    sr_select_bit: u32,
    master_clock_hz: u64,
) -> Option<Box<dyn Coprocessor>> {
    if !matches!(mode, CoprocessorMode::Lle | CoprocessorMode::Auto) {
        return None;
    }
    let fw = Dsp1Firmware::from_bytes(firmware?, ByteOrder::Auto).ok()?;
    Some(Box::new(Dsp1Chip::new_lle(
        &fw,
        master_clock_hz,
        sr_select_bit,
    )))
}

/// Without the `nec-lle` feature there is no LLE chip to build.
#[cfg(not(feature = "nec-lle"))]
fn lle_chip(
    _firmware: Option<&[u8]>,
    _mode: CoprocessorMode,
    _sr_select_bit: u32,
    _master_clock_hz: u64,
) -> Option<Box<dyn Coprocessor>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "nec-lle")]
    use crate::cartridge::firmware::ByteOrder;
    use crate::cartridge::MapTarget;
    #[cfg(feature = "nec-lle")]
    use crate::cartridge::{BusSpeed, MapPlan, MapSlot, SnesCartridge};

    // Local opcode builders (mirror the upd7725 crate's test helpers).
    #[cfg(feature = "nec-lle")]
    fn ld(id: u16, dst: u32) -> u32 {
        (3 << 22) | ((id as u32) << 6) | dst
    }
    #[cfg(feature = "nec-lle")]
    fn ljmp(addr: u32) -> u32 {
        (2 << 22) | (0x100 << 13) | (addr << 2)
    }

    #[cfg(feature = "nec-lle")]
    fn plan() -> MapPlan {
        let mut plan = MapPlan::new("dsp1-test");
        // DSP register window with the 0x4000 DR/SR-select mask.
        plan.push(
            MapSlot::new(
                0x20..=0x3F,
                0x8000..=0xFFFF,
                MapTarget::ChipMmio {
                    chip_id: 1,
                    base: 0,
                },
                BusSpeed::Fast,
            )
            .with_mask(0x4000),
        );
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        plan
    }

    #[cfg(feature = "nec-lle")]
    fn firmware(program: Vec<u32>) -> Dsp1Firmware {
        Dsp1Firmware {
            program,
            data_rom: vec![0u16; 1024],
            revision: None,
            order: ByteOrder::LittleEndian,
        }
    }

    #[cfg(feature = "nec-lle")]
    fn self_looping_program(prelude: &[u32]) -> Vec<u32> {
        // Fill with a self-loop, then drop in the prelude. Each program
        // word after the prelude jumps to itself so PC stays bounded.
        let mut program = vec![0u32; 2048];
        for (i, slot) in program.iter_mut().enumerate() {
            *slot = ljmp(i as u32);
        }
        for (i, &op) in prelude.iter().enumerate() {
            program[i] = op;
        }
        program
    }

    /// A bare [`Dsp1Chip`] (LLE) for chip-level tests, no surrounding
    /// cartridge, since these exercise the DR/SR window directly.
    #[cfg(feature = "nec-lle")]
    fn chip_with(program: Vec<u32>) -> Dsp1Chip {
        Dsp1Chip::new_lle(&firmware(program), DEFAULT_MASTER_CLOCK_HZ, 0x4000)
    }

    /// Parse a synthesized header carrying the given map-mode /
    /// cartridge-type / developer-id, so detection tests can run
    /// without hand-constructing the private `SnesHeader` fields.
    fn header_with(map_mode: u8, cart_type: u8, company: u8) -> SnesHeader {
        let mut rom = vec![0u8; 0x1_0000];
        let off = 0x7FB0;
        rom[off + 0x10..off + 0x10 + 21].copy_from_slice(b"DSP DETECT TEST  ROM ");
        rom[off + 0x25] = map_mode;
        rom[off + 0x26] = cart_type;
        rom[off + 0x27] = 0x0A;
        rom[off + 0x2A] = company;
        crate::cartridge::header::try_parse_header(&rom, off).expect("synth header parses")
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn catch_up_advances_dsp_clock_by_integer_ratio() {
        let mut chip = chip_with(self_looping_program(&[]));
        let master = 100_000u64;
        chip.catch_up(master);
        let expected = IntegerRateConverter::new(DSP1_CLOCK_HZ, DEFAULT_MASTER_CLOCK_HZ)
            .cycles_to_samples(master);
        assert_eq!(chip.dsp_cycles(), expected);
        assert!(expected > 0);
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn catch_up_is_monotonic_noop_when_behind() {
        let mut chip = chip_with(self_looping_program(&[]));
        chip.catch_up(50_000);
        let after_first = chip.dsp_cycles();
        chip.catch_up(10_000); // behind the anchor
        assert_eq!(chip.dsp_cycles(), after_first);
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn firmware_dr_load_is_visible_through_sr_and_dr_window() {
        // [0] LD #0x00AA -> DR (sets RQM); [1..] self-loop.
        let cart_prog = self_looping_program(&[ld(0x00AA, 6)]);
        let mut chip = chip_with(cart_prog);

        // Run enough DSP clocks to execute the LD at PC 0.
        chip.catch_up(1_000);

        // SR window (offset bit 14 set) reports RQM in bit 7.
        let sr = chip.read(0x4000, 1_000);
        assert_eq!(sr & 0x80, 0x80, "RQM should be set after LD ->DR");

        // DR window (offset bit 14 clear) returns the low byte first.
        let dr_lo = chip.read(0x0000, 1_000);
        assert_eq!(dr_lo, 0xAA);
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn chip_accesses_count_reads_and_writes() {
        let mut chip = chip_with(self_looping_program(&[]));
        assert_eq!(chip.chip_accesses(), 0);
        chip.write(0x0000, 0x12, 1_000); // DR write
        chip.read(0x4000, 1_000); // SR read
        chip.read(0x0000, 1_000); // DR read
        assert_eq!(chip.chip_accesses(), 3);
        assert_eq!(chip.accesses(), 3);
        // reset zeroes the access tally with the rest of the state.
        chip.reset(DEFAULT_MASTER_CLOCK_HZ);
        assert_eq!(chip.chip_accesses(), 0);
    }

    #[test]
    fn idle_stub_reports_ready_and_drops_writes() {
        let mut chip = Dsp1Chip::new_idle(0x4000);
        assert!(!chip.is_lle());
        assert_eq!(chip.read(0x4000, 0), 0x80, "SR ready");
        assert_eq!(chip.read(0x0000, 0), 0x00, "DR zero");
        chip.write(0x0000, 0x55, 0); // no panic, dropped
        assert_eq!(chip.dsp_cycles(), 0);
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn reset_chip_reanchors_and_restarts_core() {
        let mut chip = chip_with(self_looping_program(&[ld(0x1234, 1)]));
        chip.catch_up(100_000);
        assert!(chip.dsp_cycles() > 0);

        chip.reset(DEFAULT_MASTER_CLOCK_HZ);
        assert_eq!(chip.dsp_cycles(), 0, "DSP clock re-anchored to 0");

        // And it runs again from the top.
        chip.catch_up(100_000);
        assert!(chip.dsp_cycles() > 0);
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn rom_reads_delegate_to_inner_cartridge() {
        let mut rom = vec![0u8; 0x8000];
        rom[0x10] = 0x42;
        let inner = PlainCartridge::new(rom, 0, plan());
        let cart = Dsp1Cartridge::new_lle(
            inner,
            &firmware(vec![0u32; 2048]),
            DEFAULT_MASTER_CLOCK_HZ,
            0x4000,
        );
        assert_eq!(cart.read_rom(0x10), 0x42);
        assert_eq!(cart.map_plan().name, "dsp1-test");
    }

    #[test]
    fn forced_lle_that_cannot_run_is_a_stub_that_says_why() {
        let chip = select_dsp1_chip(None, CoprocessorMode::Lle, 0x4000, DEFAULT_MASTER_CLOCK_HZ);
        assert!(chip.label().contains("idle"), "{}", chip.label());
        assert_eq!(
            chip.label().contains("not built"),
            !NEC_LLE_BUILT,
            "{}",
            chip.label()
        );
    }

    #[test]
    fn auto_with_firmware_runs_the_lle_only_when_built() {
        let fw = vec![0u8; 8192];
        let chip = select_dsp1_chip(
            Some(&fw),
            CoprocessorMode::Auto,
            0x4000,
            DEFAULT_MASTER_CLOCK_HZ,
        );
        assert_eq!(
            chip.label().contains("LLE"),
            NEC_LLE_BUILT,
            "{}",
            chip.label()
        );
        assert_eq!(
            chip.label().contains("HLE"),
            !NEC_LLE_BUILT,
            "{}",
            chip.label()
        );
    }

    // -- Layout detection ----------------------------------------------

    #[test]
    fn detect_super_mario_kart_resolves_hirom() {
        // SMK ships a HiROM header: map-mode $31, cartridge type $05,
        // which is the HiROM DSP-1 board.
        let header = header_with(0x31, 0x05, 0x01);
        assert_eq!(
            detect_dsp1_layout(&header, 0x8_0000),
            Some(Dsp1Layout::HiRom)
        );
    }

    #[test]
    fn detect_lorom_dsp1_splits_on_size() {
        // Map-mode $20 + type $03 = LoROM DSP-1 (Pilotwings et al.).
        let header = header_with(0x20, 0x03, 0x01);
        assert_eq!(
            detect_dsp1_layout(&header, 0x8_0000),
            Some(Dsp1Layout::LoRom1Mb)
        );
        // > 1 MiB flips to the 2 MiB window.
        assert_eq!(
            detect_dsp1_layout(&header, 0x18_0000),
            Some(Dsp1Layout::LoRom2Mb)
        );
    }

    #[test]
    fn detect_dsp3_carveout_is_not_dsp1() {
        // Map-mode $30 + type $05 + company $B2 is DSP-3, not DSP-1.
        let dsp3 = header_with(0x30, 0x05, 0xB2);
        assert_eq!(detect_dsp1_layout(&dsp3, 0x8_0000), None);
        // Same mode/type, different company -> DSP-1 LoROM.
        let dsp1 = header_with(0x30, 0x05, 0x01);
        assert_eq!(
            detect_dsp1_layout(&dsp1, 0x8_0000),
            Some(Dsp1Layout::LoRom1Mb)
        );
    }

    #[test]
    fn detect_plain_cart_is_none() {
        // Vanilla LoROM (type $01 = ROM+RAM) is not a DSP-1 cart.
        let header = header_with(0x20, 0x01, 0x01);
        assert_eq!(detect_dsp1_layout(&header, 0x8_0000), None);
    }

    #[test]
    fn layout_sr_select_bit_per_board() {
        assert_eq!(Dsp1Layout::HiRom.sr_select_bit(), 0x1000);
        assert_eq!(Dsp1Layout::LoRom1Mb.sr_select_bit(), 0x4000);
        assert_eq!(Dsp1Layout::LoRom2Mb.sr_select_bit(), 0x4000);
    }

    #[test]
    fn hirom_register_slots_cover_smk_windows() {
        let [low, high] = Dsp1Layout::HiRom.register_slots(DSP1_CHIP_ID);
        assert_eq!(low.bank_range, 0x00..=0x1F);
        assert_eq!(low.offset_range, 0x6000..=0x7FFF);
        assert_eq!(low.mask, Some(0x1000));
        assert_eq!(high.bank_range, 0x80..=0x9F);
        assert_eq!(high.offset_range, 0x6000..=0x7FFF);
        assert!(matches!(
            low.target,
            MapTarget::ChipMmio {
                chip_id: DSP1_CHIP_ID,
                base: 0
            }
        ));
    }

    /// End-to-end DR/SR decode for the HiROM bit-12 layout: build a chip
    /// with the HiROM select bit and confirm `$7000` hits SR while `$6000`
    /// hits DR. The slot mask reduces the bus offset to `{0, 0x1000}`, so
    /// we exercise the chip with those values.
    #[cfg(feature = "nec-lle")]
    #[test]
    fn hirom_dr_sr_decode_uses_bit_12() {
        let prog = self_looping_program(&[ld(0x00BB, 6)]); // LD #0x00BB -> DR
        let mut chip = Dsp1Chip::new_lle(&firmware(prog), DEFAULT_MASTER_CLOCK_HZ, 0x1000);
        chip.catch_up(1_000);

        // SR via the bit-12 window (offset 0x1000).
        let sr = chip.read(0x1000, 1_000);
        assert_eq!(sr & 0x80, 0x80, "RQM set after LD ->DR");
        // DR via offset 0 returns the low byte.
        assert_eq!(chip.read(0x0000, 1_000), 0xBB);
        // The label reflects the LLE backend.
        assert!(chip.label().contains("LLE"));
    }

    // -- Savestate + debugger surfacing ----------------------------------

    #[cfg(feature = "nec-lle")]
    #[test]
    fn coprocessor_savestate_round_trips_core_and_anchor() {
        // LD #0x1234 -> DR then self-loop; run far enough to execute it
        // and advance the converter anchor past zero.
        let mut chip = chip_with(self_looping_program(&[ld(0x1234, 6)]));
        chip.catch_up(500_000);
        // Poke a known data-RAM pattern so the round-trip covers RAM too.
        if let Backend::Lle { core, .. } = &mut chip.backend {
            core.data_ram[0] = 0xCAFE;
            core.data_ram[255] = 0x1357;
        }

        let saved = chip.savestate().expect("LLE chip yields a savestate");
        let saved_dsp = match &saved {
            CoprocessorState::Dsp1Lle(s) => s.clone(),
            other => panic!("expected an LLE savestate, got {other:?}"),
        };

        // Mutate the live chip *away* from the snapshot so the restore has
        // to overwrite it (reset_state leaves data RAM untouched, so we
        // clobber the RAM words by hand here).
        if let Backend::Lle {
            core, dsp_cycles, ..
        } = &mut chip.backend
        {
            core.program_counter = 0x7FF;
            core.data_port = 0;
            core.data_ram[0] = 0;
            core.data_ram[255] = 0;
            *dsp_cycles = 0;
        }

        chip.restore_savestate(&saved, DEFAULT_MASTER_CLOCK_HZ);

        match &chip.backend {
            Backend::Lle {
                core,
                sync,
                dsp_cycles,
            } => {
                assert_eq!(core.program_counter, saved_dsp.core.pc, "PC restored");
                assert_eq!(core.data_port, saved_dsp.core.dr, "DR restored");
                assert_eq!(core.data_ram[0], 0xCAFE, "data RAM restored");
                assert_eq!(core.data_ram[255], 0x1357);
                assert_eq!(*dsp_cycles, saved_dsp.dsp_cycles, "dsp_cycles restored");
                // Converter anchor (the three persisted fields) round-trips.
                assert_eq!(sync.reference_cycles(), saved_dsp.sync.reference_cycles);
                assert_eq!(
                    sync.samples_at_reference(),
                    saved_dsp.sync.samples_at_reference
                );
                assert_eq!(sync.remainder(), saved_dsp.sync.remainder);
            }
            Backend::Idle => panic!("expected LLE backend"),
        }
    }

    #[test]
    fn idle_stub_has_no_savestate_and_restore_is_noop() {
        let mut chip = Dsp1Chip::new_idle(0x4000);
        assert!(chip.savestate().is_none());

        // A foreign blob restored into the idle stub is dropped silently.
        let blob = CoprocessorState::Dsp1Lle(crate::savestate::Dsp1State {
            core: crate::savestate::Upd7725State::blank(),
            sync: crate::savestate::ConverterState::default(),
            dsp_cycles: 123,
        });
        chip.restore_savestate(&blob, DEFAULT_MASTER_CLOCK_HZ);
        assert_eq!(chip.dsp_cycles(), 0);
    }

    #[cfg(feature = "nec-lle")]
    #[test]
    fn coprocessor_debug_state_surfaces_dsp1_registers_and_ram() {
        let mut chip = chip_with(self_looping_program(&[ld(0x00AA, 6)]));
        chip.catch_up(2_000);
        let st = chip.debug_state();
        assert_eq!(st.chip_type, ExpansionChipType::DSP1);
        assert!(st.program_counter.is_some());
        // 256 words x 2 bytes, little-endian.
        assert_eq!(st.internal_ram.as_ref().map(|r| r.len()), Some(512));
        let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
        for want in ["PC", "SR", "DR", "A", "B", "FLAGA", "FLAGB", "ACCESSES"] {
            assert!(names.contains(&want), "missing register {want}");
        }
    }

    #[test]
    fn idle_stub_debug_state_is_present_but_empty() {
        let chip = Dsp1Chip::new_idle(0x4000);
        let st = chip.debug_state();
        assert_eq!(st.chip_type, ExpansionChipType::DSP1);
        assert!(st.registers.is_empty());
        assert!(st.internal_ram.is_none());
        assert_eq!(st.program_counter, None);
        assert_eq!(st.cycle_count, 0);
    }
}
