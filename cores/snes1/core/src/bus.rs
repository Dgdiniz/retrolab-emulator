//! SNES main bus.
//!
//! Decodes the 24-bit address space exposed by the Ricoh 5A22 into
//! categories of access (WRAM, WRAM mirror, B-bus window, CPU MMIO,
//! cartridge slot, open bus) via a flat page table indexed by the
//! top 12 bits of the address. The hot path is two indexed loads
//! (page-table entry plus the dispatch target) and one branch, no
//! per-mapper match anywhere.
//!
//! Architecturally the bus reflects the dual-bus structure of the
//! real console:
//!
//! - **Bus-A** (24 address bits + 8 data bits) is the main bus that
//!   the 65C816 talks to. It is what callers see through
//!   [`Addressable::read`] and [`Addressable::write`].
//! - **Bus-B** (8 address bits + 8 data bits) is a physically
//!   separate path that the PPU and APU sit on. Inside the bus it
//!   lives as a private [`b_bus::BBus`]; reads in
//!   `$xx:2100-21FF` are routed through it via the A-bus dispatch,
//!   and so are DMA's bytes, which the DMA engine moves through the
//!   same dispatch at `$2100 | port`. The `b_bus_dma_*` entry points,
//!   which skip the A-bus decoder, are stubs nothing calls: a read
//!   returns 0 and a write is dropped.
//!
//! Cartridge mapping is delegated to a [`SnesCartridge`]
//! installed once via [`SnesBus::install_cartridge`]. The bus walks
//! the cartridge's [`MapPlan`] at install time and rewrites the
//! covered pages to [`PageHandler::Cartridge`] under a *first-wins*
//! policy: only pages currently marked [`PageHandler::OpenBus`] are
//! promoted, which both protects system pages (`Wram`, `WramMirror`,
//! `BBus`, `CpuMmio`) and ensures the first slot covering a page
//! wins against later overlapping slots. After install the bus does
//! not know the cartridge variant; every cartridge access routes
//! through the trait. LoROM, HiROM, ExHiROM, and arbitrary homebrew
//! layouts therefore reuse the same dispatch code.
//!
//! Alongside the [`PageHandler`] page table the bus also keeps a
//! parallel `speed_table` of master-cycle costs per 4 KiB page,
//! Slow (8 mclks) for every page by default. The system area of
//! the I/O banks (`$0000-$7FFF`) bypasses the table:
//! [`SnesBus::cycles_for`] charges 8 for the WRAM mirror, 6 for
//! `$2000-$3FFF`, 12 for the `$4000-$41FF` joypad ports, 6 for the
//! `$4200-$5FFF` registers and 8 for `$6000-$7FFF`. Slots installed
//! by [`SnesBus::install_cartridge`] overwrite the speed of their
//! pages with `slot.speed.master_cycles()`. The MEMSEL bit
//! (`$420D` bit 0) toggles FastROM on/off for the pages of the
//! FastROM region, `$80-$BF:$8000-$FFFF` and the whole of banks
//! `$C0-$FF`, that come from cartridge slots declared as
//! [`BusSpeed::Fast`]; the bus caches the set of FastROM-eligible
//! pages at install time and rewrites those entries on every
//! [`SnesBus::apply_memsel`] call.
//!
//! The `$2180-$2183` WMDATA / WMADD registers belong to the 5A22
//! (not to the PPU or APU), so the bus intercepts them in the
//! B-bus window before delegating to the internal [`b_bus::BBus`]
//! dispatcher. The same pattern applies to OPHCT / OPVCT
//! (`$213C` / `$213D`) and the STAT78 read-side-effect on `$213F`:
//! the bus drives the H/V latches via the [`crate::system_regs::SystemRegs`]
//! it owns, then passes the read through to the B-bus for the
//! actual register byte.
//!
//! `$4200-$420F` (system register writes) and `$4210-$421F`
//! (system register reads) dispatch through `read_cpu_mmio` /
//! `write_cpu_mmio` into [`crate::system_regs::SystemRegs`]. Other
//! addresses in the CPU MMIO page go to the two controller ports
//! (`$4016/$4017`, the manual joypad) and to the DMA controller
//! (`$4300-$437F`, the channel registers).
//!
//! Bus accesses come in three tiers, [`SnesBus::read_fast`],
//! [`SnesBus::read_light`], and [`SnesBus::read_full`] (plus the
//! `write_*` analogues), one per [`retro_core::debug::ExecutionTier`].
//! All three tiers delegate to the same internal dispatcher. The CPU
//! steps against the [`retro_core::cpu::Addressable`] impl on
//! `SnesBus`, which calls `read_full` / `write_full` (the safe default)
//! and, around each access, adds its master-cycle cost and records what
//! the debugger and the diagnostic traces need.

mod b_bus;
mod debug;
mod dma;
mod wmdata;

use std::sync::Arc;

use retro_core::capture::{CaptureRecording, WriteOrigin};
use retro_core::cartridge::{Cartridge, SlotShape, SourcePlace};
use retro_core::cpu::Addressable;
use retro_core::debug::{AccessLog, RaisedEvents};

use crate::apu::Apu;
use crate::cartridge::{BusSpeed, MapPlan, MapTarget, SnesCartridge, CX4_CHIP_ID};
use crate::controller::StandardPad;
use crate::dma::DmaController;
use crate::ppu::{Ppu, PpuAccessCtx};
use crate::system_regs::{SystemRegWriteFx, SystemRegs};

use b_bus::BBus;
use debug::IoTraceBuf;
pub use debug::{BusDebugEvent, IoAccess};
use wmdata::WmdataState;

/// PPU2 hardware version exposed in bits 0-3 of `$213F` STAT78.
/// The original SHVC-CPU shipped with PPU2 v1; later 1-Chip
/// revisions bumped it. We default to 1, the value the reference
/// traces were made with.
const PPU2_VERSION: u8 = 1;

/// Number of pages in the address space (16 MiB / 4 KiB = 4096).
const PAGE_TABLE_LEN: usize = 0x1000;

/// The write events the bus raises: one for each of the 64 PPU registers
/// and the 4 APU ports. Each is kept once between two drains, so the list
/// never holds more.
const BUS_WRITE_EVENTS: usize = 0x40 + 4;

/// Resolve a CPU address to its physical WRAM offset (`0..0x20000`), or
/// `None` if it doesn't land in WRAM. Handles both the `$7E/$7F` direct
/// banks and the low-8 KiB (`$0000-$1FFF`) mirror present in every
/// system-area bank (`$00-$3F` / `$80-$BF`). Used by the address watch
/// so a watched WRAM cell is caught no matter which alias the game uses.
#[inline]
fn wram_offset(addr: u32) -> Option<u32> {
    let bank = (addr >> 16) & 0xFF;
    let off = addr & 0xFFFF;
    match bank {
        0x7E..=0x7F => Some(((bank - 0x7E) << 16) | off),
        0x00..=0x3F | 0x80..=0xBF if off < 0x2000 => Some(off),
        _ => None,
    }
}

/// 24-bit SNES address space dispatcher.
///
/// Owns the 128 KiB of WRAM physically present in the 5A22, the
/// page table covering the 16 MiB address space, a parallel
/// `speed_table` of master-cycle costs per page, the MDR latch that
/// backs open-bus behaviour, the internal B-bus, the WMDATA / WMADD
/// state, and optionally a cartridge installed through
/// [`SnesBus::install_cartridge`].
pub struct SnesBus {
    wram: Box<[u8; 0x20000]>,
    page_table: Box<[PageHandler; PAGE_TABLE_LEN]>,
    /// Master-cycle cost per 4 KiB page. Parallel to `page_table`
    /// and indexed the same way. Defaults are set by
    /// [`build_default_speed_table`]; cartridge slot speeds
    /// overwrite the covered pages at install time.
    speed_table: Box<[u8; PAGE_TABLE_LEN]>,
    mdr: u8,
    b_bus: BBus,
    /// WMDATA / WMADD state. The four registers at
    /// `$2180-$2183` are on the 5A22 side of the B-bus window, so
    /// the bus intercepts them before delegating to `b_bus`.
    wmdata: WmdataState,
    /// 5A22 system registers (`$4200-$421F`). Dispatched from
    /// `read_cpu_mmio` / `write_cpu_mmio`. The bus also owns the
    /// H/V counter latches stored inside `SystemRegs`; the
    /// `$213C/$213D/$213F` traffic is intercepted in
    /// `read_b_bus_window` and routed through the same object.
    system_regs: SystemRegs,
    /// Current MEMSEL bit 0 (`$420D`). Mirrored from
    /// `system_regs.memsel_fast()` so the bus can read it without
    /// reaching into `SystemRegs` on every cycle.
    memsel_fast: bool,
    /// Cartridge pages in `$80-$BF:$8000-$FFFF` whose slot speed
    /// is `BusSpeed::Fast`, the FastROM-eligible set. Populated
    /// by [`SnesBus::rebuild_fastrom_pages`] at install time;
    /// rewritten by [`SnesBus::apply_memsel`] whenever MEMSEL
    /// flips. Pages outside this list are unaffected by MEMSEL.
    fastrom_pages: Vec<u16>,
    cartridge: Option<Box<SnesCartridge>>,
    /// Cached pointer to the cartridge's map plan. `Arc` keeps the
    /// plan alive even if the cartridge swaps out its own internal
    /// state, and lets the bus borrow slot metadata without locking
    /// down the cartridge for write access, important because chip
    /// reads take `&mut self`. Set when a cartridge is installed.
    cartridge_plan: Option<Arc<MapPlan>>,
    /// The installed cartridge maps a coprocessor MMIO window inside the
    /// CpuMmio expansion page (`$4800-$48FF`), which the page-table walker
    /// refuses to promote (a whole 4 KiB page is `CpuMmio`). When `true`,
    /// `read/write_cpu_mmio` carve that sub-range out to the cartridge via
    /// the map plan. Computed once at install time; `false` for every plain
    /// cartridge, so the carve-out branch never fires for stock ROMs.
    cart_claims_expansion_mmio: bool,
    /// The installed cartridge maps a coprocessor MMIO window inside the B-bus
    /// page (`$2200-$23FF`), the SA-1 register file. Like
    /// `cart_claims_expansion_mmio` but for the B-bus window: the page-table
    /// walker refuses to promote it (the whole `$2000` page is the B-bus
    /// window), so `read/write_b_bus_window` carve that sub-range out to the
    /// cartridge via the map plan. Computed once at install time; `false` for
    /// every plain cartridge, so the carve-out branch never fires for stock
    /// ROMs.
    cart_claims_bbus_mmio: bool,
    /// The installed cartridge runs a coprocessor CPU by burst: the SA-1's
    /// second 65816, the Super FX's GSU or the Cx4. When `true`, `system.rs`
    /// catches the coprocessor up once per S-CPU instruction (not just at
    /// `FrameEnd`), so the two CPUs stay in step across stretches with no
    /// shared-memory traffic. Set at install time from the chip's MMIO window
    /// (the SA-1's `$2200-$23FF` B-bus claim, the Super FX's `$3000-$34FF`,
    /// the Cx4's `$6000-$7FFF`); `false` for every other cartridge, so their
    /// per-instruction path pays only one predictable, always-false branch.
    cart_runs_burst_cpu: bool,
    /// A chip on the installed cartridge registered a passive bus observer
    /// (e.g. the S-DD1 spying on `$43xx`). When `true`, the `$43xx` write
    /// handler forwards the write to the cartridge after the DMA controller
    /// consumes it. Computed once at install time; `false` for every plain
    /// cartridge, so the forward never fires for stock ROMs.
    cart_observes_bus: bool,
    /// Opt-in capture of the host<->coprocessor register stream. When
    /// `Some`, every `ChipMmio` access records `(is_write, offset, byte)`:
    /// the post-slot-mask offset (`0` = DR, `sr_select_bit` = SR) and
    /// the byte written or read back. `None` (the default) adds a single
    /// predictable branch on the cartridge path and no allocation, so
    /// normal play is unaffected. It lets a real game's command stream be
    /// captured and replayed through the DSP-1 HLE backend.
    coprocessor_trace: Option<Vec<(bool, u32, u8)>>,
    /// Opt-in diagnostic ring buffer of synchronisation-register
    /// accesses (see [`IoAccess`]). `None` (the default) costs one
    /// predictable, almost-always-false branch on the CPU read/write
    /// path and never allocates, so normal play is unaffected. Used by
    /// `ppu-dump --io-trace` to locate the spin loop a stalled game is
    /// parked in.
    io_trace: Option<IoTraceBuf>,
    /// Opt-in ring buffer of the most recent instruction program
    /// counters. Companion to [`Self::io_trace`] for locating a spin
    /// loop the *main thread* is parked in, a tight loop that touches
    /// no watched MMIO register is invisible to the I/O trace but
    /// dominates this PC histogram. `None` by default.
    pc_trace: Option<(std::collections::VecDeque<u32>, usize)>,
    /// `PBR:PC` of the instruction currently executing, stamped onto
    /// each [`IoAccess`] and pushed into [`Self::pc_trace`]. `system.rs`
    /// refreshes it before stepping the CPU, but only while
    /// [`Self::diag_on`] says something reads it; the fast path never
    /// writes it.
    trace_pc: u32,
    /// A diagnostic trace is on, or a recorder of the PPU wants the
    /// processor named as the writer of its stores: the one flag
    /// `system.rs` tests before each instruction to stamp
    /// [`Self::trace_pc`]. Kept up by every call that turns one of them on
    /// or off ([`Self::refresh_diag_on`]).
    diag_on: bool,
    /// Opt-in watch on a single WRAM cell: `(wram_offset, ring)`. Records
    /// every read/write that resolves to that physical WRAM offset,
    /// through the `$7E/$7F` direct banks *or* the low-8 KiB mirror in
    /// the system banks, so a game-state variable (e.g. a cutscene VM
    /// counter) can be traced to the exact PC that touches it. `None`
    /// by default; same near-free guarded branch as [`Self::io_trace`].
    addr_watch: Option<(u32, IoTraceBuf)>,
    /// Cached raster snapshot, last `(h_dot, v_scanline)` reported
    /// by the scheduler via [`SnesBus::set_raster_snapshot`]. The
    /// PPU dispatch (B-bus side) needs `(h, v)` to gate active-scan
    /// OAM/CGRAM access; the `$2137` SLHV intercept needs them to
    /// latch the H/V counters; the WRIO falling-edge `LatchHv`
    /// side-effect also reads from this cache. The emulator's main
    /// loop refreshes the snapshot at the end of every CPU step so
    /// the staleness is bounded by one instruction.
    cached_h_dot: u16,
    /// Companion to [`Self::cached_h_dot`].
    cached_v_scanline: u16,
    /// The eight DMA/HDMA channels and their `$43xx` register file.
    /// General DMA runs atomically at the instruction boundary; HDMA
    /// runs from scheduler events. See the `dma` submodule.
    dma: DmaController,
    /// MDMAEN (`$420B`) mask captured by a CPU write but not yet
    /// executed. `system.rs` drains it after the current instruction
    /// finishes and runs [`SnesBus::run_general_dma`]. Zero means no
    /// general DMA is pending.
    pending_general_dma: u8,
    /// Set when a `$4200` (NMITIMEN) or `$4207-$420A` (HTIME/VTIME)
    /// write changes the IRQ configuration. `system.rs` drains it
    /// after the instruction and re-arms the H/V IRQ events from the
    /// current frame base.
    pending_irq_reschedule: bool,
    /// Set when a `$4200` write enables NMI (0->1 edge) while the
    /// `$4210` NMI flag is still latched, the re-trigger quirk.
    /// `system.rs` folds it into the CPU's `nmi_pending` because the
    /// bus cannot reach the CPU directly.
    pending_nmi: bool,
    /// The audio subsystem (SPC700 + S-DSP + ARAM). Owned here so a
    /// `$2140-$2143` access can catch the SPC700 up to the current
    /// master cycle *before* reading or writing the mailbox port,
    /// the CPU read must observe the SPC's latest write.
    apu: Apu,
    /// Master cycle the APU should be caught up to on the next port
    /// access. Refreshed once per CPU instruction by `system.rs`
    /// (alongside the raster snapshot), so APU staleness during an
    /// access is bounded by one instruction.
    apu_cycle: u64,
    /// The two controller ports. Port 0 is `$4016` / JOY1, port 1 is
    /// `$4017` / JOY2. `$4016` writes strobe both. The held-button
    /// state is refreshed once per frame by `system.rs` from the input
    /// manager via [`SnesBus::set_controller_buttons`].
    controllers: [StandardPad; 2],
    /// When set, the CPU's bus reads and writes are logged in
    /// [`Self::accesses`] and its writes to the PPU registers and the APU
    /// ports in [`Self::debug_events`], so the emulator can match
    /// watchpoints and those write events after the instruction. The
    /// debugger arms this (via [`SnesBus::set_debug_armed`]) only when at
    /// least one watch/event breakpoint exists, so the fast path pays
    /// nothing.
    debug_armed: bool,
    /// The bytes the CPU read and wrote this instruction, for the
    /// watchpoints (checked and cleared per step by `system.rs`). Always
    /// empty unless [`Self::debug_armed`] is set.
    accesses: AccessLog,
    /// The PPU-register and APU-port writes the CPU made this instruction,
    /// each once, for their event breakpoints. Always empty unless
    /// [`Self::debug_armed`] is set.
    debug_events: RaisedEvents<BusDebugEvent>,
    /// Master cycles accumulated by the CPU's memory accesses since the
    /// last [`Self::begin_cpu_instruction`]. Each `read`/`write` on the
    /// [`Addressable`] trait adds [`Self::cycles_for`] of the touched
    /// address. `system.rs` reads it via [`Self::take_cpu_instruction_cost`]
    /// to charge the instruction its real per-access master-cycle cost
    /// (6/8/12) instead of a flat FastROM rate. DMA/HDMA go through
    /// `read_inner`/`write_inner` and so never touch this counter.
    instr_master_cycles: u64,
    /// Number of CPU memory accesses counted alongside
    /// [`Self::instr_master_cycles`]. The difference between the CPU's
    /// reported cycle count and this access count is the instruction's
    /// internal (non-bus) cycles, charged at the 6 mclk fast rate.
    instr_accesses: u32,
    /// Master cycle of the next H/V IRQ, handed in by `system.rs` for
    /// the instruction about to run and cleared after it (see
    /// [`Self::set_next_irq_cycle`]).
    next_irq_cycle: Option<u64>,
    /// Master cycle of the IRQ a `$4211` read saw and acknowledged before
    /// its event fired, taken by `system.rs` after the instruction
    /// through [`Self::take_irq_read_early`].
    irq_read_early: Option<u64>,
}

/// Category of access for one page of the address space.
///
/// Every entry in [`SnesBus::page_table`] is one of these variants.
/// Variants without payload denote a fully-known subsystem; the
/// `Cartridge` variant carries the slot index so the read/write
/// path can recover the [`crate::cartridge::MapSlot`] without
/// re-walking the cartridge's plan.
///
/// `BBus` and `CpuMmio` cover pages that *contain* the relevant
/// MMIO window but also some open-bus bytes (the B-bus window is
/// only 256 bytes wide inside a 4 KiB page; the same is true for
/// the $4200-$43FF MMIO range). The corresponding handlers filter
/// the exact sub-range and fall back to MDR for misses, which
/// keeps the page table 16x smaller than a byte-granular table
/// without losing fidelity.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PageHandler {
    /// Page inside `$7E:0000-$7F:FFFF`: direct WRAM access.
    Wram,
    /// First 8 KiB of WRAM mirrored at `$xx:0000-$xx:1FFF` in the
    /// system-area banks `$00-$3F` and `$80-$BF`.
    WramMirror,
    /// Page containing the B-bus window at `$2100-$21FF`.
    BBus,
    /// Page containing CPU MMIO at `$4200-$43FF` (plus manual
    /// joypad registers at `$4016/$4017` and the XSlow joypad
    /// registers at `$4000-$41FF`).
    CpuMmio,
    /// Page resolved through the installed cartridge's
    /// [`MapPlan`]. The `slot_index` indexes into
    /// `MapPlan::slots`.
    Cartridge { slot_index: u16 },
    /// Page with nothing mapped. Reads return MDR; writes latch MDR
    /// but otherwise drop.
    OpenBus,
}

/// Whether a B-bus page address is one of the PPU's three data ports
/// (`$2104` OAMDATA, `$2118/$2119` VMDATA, `$2122` CGDATA), the writes the
/// recorders of video memory note.
#[inline]
fn is_noted_port(addr24: u32) -> bool {
    matches!(addr24 & 0xFFFF, 0x2104 | 0x2118 | 0x2119 | 0x2122)
}

/// Whether a CPU address reaches a PPU register: `$2100-$213F` of the
/// system-area B-bus window (banks `$00-$3F` / `$80-$BF`), the range
/// `classify_b_bus_write` reports as PPU register writes.
pub(crate) fn is_ppu_register_address(addr: u32) -> bool {
    let bank = (addr >> 16) & 0xFF;
    (bank <= 0x3F || (0x80..=0xBF).contains(&bank)) && (0x2100..=0x213F).contains(&(addr & 0xFFFF))
}

impl Default for SnesBus {
    fn default() -> Self {
        Self::new()
    }
}

impl SnesBus {
    /// Build a bus with cleared WRAM, MDR at 0, an unwired B-bus
    /// stub, no cartridge installed, the default page table, the
    /// default speed table, and zeroed system registers (MEMSEL
    /// off, matching the SNES boot state).
    pub fn new() -> Self {
        Self {
            wram: Box::new([0u8; 0x20000]),
            page_table: Box::new(build_default_page_table()),
            speed_table: Box::new(build_default_speed_table()),
            mdr: 0,
            b_bus: BBus::new(),
            wmdata: WmdataState::new(),
            system_regs: SystemRegs::new(),
            memsel_fast: false,
            fastrom_pages: Vec::new(),
            cartridge: None,
            cartridge_plan: None,
            cart_claims_expansion_mmio: false,
            cart_claims_bbus_mmio: false,
            cart_runs_burst_cpu: false,
            cart_observes_bus: false,
            coprocessor_trace: None,
            io_trace: None,
            pc_trace: None,
            trace_pc: 0,
            diag_on: false,
            addr_watch: None,
            cached_h_dot: 0,
            cached_v_scanline: 0,
            dma: DmaController::new(),
            pending_general_dma: 0,
            pending_irq_reschedule: false,
            pending_nmi: false,
            apu: Apu::new(),
            apu_cycle: 0,
            controllers: [StandardPad::new(); 2],
            debug_armed: false,
            accesses: AccessLog::new(),
            debug_events: RaisedEvents::new(BUS_WRITE_EVENTS),
            instr_master_cycles: 0,
            instr_accesses: 0,
            next_irq_cycle: None,
            irq_read_early: None,
        }
    }

    /// Reset the per-instruction master-cycle accumulator. `system.rs`
    /// calls this immediately before `cpu.step` so the subsequent
    /// [`Addressable`] reads/writes are attributed to exactly one CPU
    /// instruction.
    #[inline]
    pub fn begin_cpu_instruction(&mut self) {
        self.instr_master_cycles = 0;
        self.instr_accesses = 0;
    }

    /// Take the master-cycle cost and access count accumulated by the
    /// CPU's memory accesses since the last
    /// [`Self::begin_cpu_instruction`]. Returns
    /// `(accumulated_master_cycles, access_count)`; the caller charges
    /// the leftover internal cycles (`cpu_cycles - access_count`) at the
    /// 6 mclk fast rate. Does not reset, the next
    /// `begin_cpu_instruction` does.
    #[inline]
    pub fn take_cpu_instruction_cost(&self) -> (u64, u32) {
        (self.instr_master_cycles, self.instr_accesses)
    }

    /// Read-only borrow of the DMA/HDMA controller. Used by tests and
    /// the debugger to inspect channel register state.
    pub fn dma(&self) -> &DmaController {
        &self.dma
    }

    /// Mutable borrow of the DMA/HDMA controller.
    pub fn dma_mut(&mut self) -> &mut DmaController {
        &mut self.dma
    }

    /// Read-only borrow of the system registers (`$4200-$421F`).
    pub fn system_regs(&self) -> &SystemRegs {
        &self.system_regs
    }

    /// Mutable borrow of the system registers. Used by the
    /// scheduler / IRQ subsystems to set NMI / IRQ / HVBJOY flags
    /// without going through the bus dispatch.
    pub fn system_regs_mut(&mut self) -> &mut SystemRegs {
        &mut self.system_regs
    }

    /// Push the held-button state into a controller port (`0` or `1`),
    /// already in the SNES hardware layout. Out-of-range ports are
    /// ignored. Called by `system.rs` from the input manager (see
    /// `input_state_to_snes_pad`) at every `set_input` and again at
    /// the start of V-blank.
    pub fn set_controller_buttons(&mut self, port: usize, buttons: u16) {
        if let Some(pad) = self.controllers.get_mut(port) {
            pad.set_buttons(buttons);
        }
    }

    /// Run a hardware auto-joypad read: strobe both ports, snapshot the
    /// 16-bit pad state into JOY1 (`$4218/$4219`) and JOY2
    /// (`$421A/$421B`), zero the multitap-only JOY3/JOY4 bytes, and
    /// raise the HVBJOY "joypad busy" flag (`$4212` bit 0).
    ///
    /// Triggered from `VBlankStart` when `NMITIMEN` bit 0 is set;
    /// `system.rs` schedules [`SnesBus::auto_joypad_finish`] to clear
    /// the busy flag after the read window.
    pub fn auto_joypad_read(&mut self) {
        // The strobe pulse latches each pad's shift register, mirroring
        // the hardware sequence even though `buttons` reads the raw
        // snapshot directly.
        for pad in &mut self.controllers {
            pad.strobe(true);
            pad.strobe(false);
        }
        let joy1 = self.controllers[0].buttons();
        let joy2 = self.controllers[1].buttons();
        // Hardware clocks each pad 16 times while filling JOY1-JOY4,
        // leaving the serial shift register exhausted. Mirror that so a
        // *manual* $4016/$4017 read the game makes afterwards, without
        // re-strobing, relying on the auto-read to have strobed for it,
        // returns the idle data-line bit (1), not stale button data.
        // Dark Law depends on this: its NMI input routine polls $4016
        // bit 0 directly and misreads a live B-button bit as the data
        // line if the register is left primed at bit 0.
        for pad in &mut self.controllers {
            pad.exhaust_serial();
        }
        // JOY1 low/high -> $4218/$4219, JOY2 low/high -> $421A/$421B.
        self.system_regs.set_joy_byte(0, joy1 as u8);
        self.system_regs.set_joy_byte(1, (joy1 >> 8) as u8);
        self.system_regs.set_joy_byte(2, joy2 as u8);
        self.system_regs.set_joy_byte(3, (joy2 >> 8) as u8);
        // JOY3/JOY4 ($421C-$421F) are multitap-only; no device -> 0.
        for index in 4..8 {
            self.system_regs.set_joy_byte(index, 0);
        }
        self.system_regs.set_joypad_busy(true);
    }

    /// Clear the HVBJOY "joypad busy" flag once the auto-read window
    /// elapses. Scheduled by `system.rs` after [`auto_joypad_read`].
    ///
    /// [`auto_joypad_read`]: SnesBus::auto_joypad_read
    pub fn auto_joypad_finish(&mut self) {
        self.system_regs.set_joypad_busy(false);
    }

    /// Reset both controller ports to their power-on state (no buttons
    /// held, latch line low, shift register cleared). Called from the
    /// emulator's `reset`.
    pub fn reset_controllers(&mut self) {
        self.controllers = [StandardPad::new(); 2];
    }

    /// Take and clear the pending IRQ-reschedule flag (set by a
    /// `$4200` / `$4207-$420A` write). When this returns `true`,
    /// `system.rs` cancels the stale H/V IRQ events and re-arms them.
    pub fn take_pending_irq_reschedule(&mut self) -> bool {
        let pending = self.pending_irq_reschedule;
        self.pending_irq_reschedule = false;
        pending
    }

    /// Take and clear the pending NMI re-trigger flag (set by a
    /// `$4200` NMI-enable 0->1 edge while `$4210` is latched).
    /// `system.rs` folds this into the CPU's `nmi_pending`.
    pub fn take_pending_nmi(&mut self) -> bool {
        let pending = self.pending_nmi;
        self.pending_nmi = false;
        pending
    }

    /// Current MEMSEL FastROM-enable bit. Mirrored from `SystemRegs`
    /// for the hot path.
    pub fn memsel_fast(&self) -> bool {
        self.memsel_fast
    }

    /// Current MDR (memory data register) latch.
    pub fn mdr(&self) -> u8 {
        self.mdr
    }

    /// Read-only borrow of the PPU. The PPU owns VRAM, OAM,
    /// CGRAM, and the full `$2100-$213F` register state.
    pub fn ppu(&self) -> &Ppu {
        self.b_bus.ppu()
    }

    /// Mutable borrow of the PPU. Used by the debugger and by
    /// scheduler hooks (e.g. the `VBlankStart` handler that toggles
    /// the interlace field and resets the OAM address).
    pub fn ppu_mut(&mut self) -> &mut Ppu {
        self.b_bus.ppu_mut()
    }

    /// Read-only borrow of the APU (SPC700 + S-DSP + ARAM).
    pub fn apu(&self) -> &Apu {
        &self.apu
    }

    /// Mutable borrow of the APU. Used by `system.rs` to catch the APU
    /// up at frame boundaries and to re-anchor it on reset.
    pub fn apu_mut(&mut self) -> &mut Apu {
        &mut self.apu
    }

    /// Record the master cycle the APU should catch up to on the next
    /// mailbox port access. Called once per CPU instruction by the
    /// main loop, mirroring [`SnesBus::set_raster_snapshot`].
    pub fn set_apu_cycle(&mut self, master_cycle: u64) {
        self.apu_cycle = master_cycle;
    }

    /// Master cycle a mailbox access should catch the APU up to.
    ///
    /// `apu_cycle` is the master cycle at the *end of the previous CPU
    /// instruction*; `instr_master_cycles` is what the current
    /// instruction has accrued up to (and including) this access, the
    /// `read`/`write` trait methods add the access's cost before
    /// dispatching here. Their sum is the precise sub-instruction master
    /// cycle of the access: the clock has advanced by the access's cost
    /// before the port is touched. Using it (instead of the stale
    /// `apu_cycle`) keeps the SPC700 caught up to the exact instant the
    /// 5A22 touches the port, which the command/response handshakes
    /// between games and their sound drivers depend on.
    #[inline]
    fn apu_access_cycle(&self) -> u64 {
        self.apu_cycle + self.instr_master_cycles
    }

    /// Hand the bus the master cycle of the next H/V IRQ for the
    /// instruction about to run, `None` when no IRQ is scheduled.
    ///
    /// On hardware the TIMEUP flag (`$4211` bit 7) goes up at the IRQ's
    /// raster point, while the CPU takes the interrupt only after the
    /// instruction in flight. The scheduler raises the flag between
    /// instructions, so a read inside the instruction during which the
    /// IRQ rises needs this cycle to see it. A wait loop that polls
    /// TIMEUP while an IRQ handler also acknowledges it can leave only
    /// through such a read.
    pub fn set_next_irq_cycle(&mut self, cycle: Option<u64>) {
        self.next_irq_cycle = cycle;
    }

    /// After an instruction: the master cycle of the IRQ a `$4211` read
    /// inside it saw before its event fired, if any. Also clears the
    /// cycle [`Self::set_next_irq_cycle`] handed in.
    pub fn take_irq_read_early(&mut self) -> Option<u64> {
        self.next_irq_cycle = None;
        self.irq_read_early.take()
    }

    /// Master cycle of a `$4211` (TIMEUP) read, with the same
    /// sub-instruction precision as [`Self::apu_access_cycle`].
    #[inline]
    fn timeup_access_cycle(&self) -> u64 {
        self.apu_cycle + self.instr_master_cycles
    }

    /// Master cycle a cartridge chip (DSP-1, ...) access should catch its
    /// coprocessor up to. Same sub-instruction precision as
    /// [`Self::apu_access_cycle`]: the per-instruction base plus the
    /// master cycles this instruction has accrued up to and including the
    /// access. A coprocessor whose only CPU-observable state is its
    /// register file is then ordered exactly against the 5A22 touch.
    #[inline]
    fn chip_access_cycle(&self) -> u64 {
        self.apu_cycle + self.instr_master_cycles
    }

    /// Advance the cartridge coprocessor (if any) to `master_cycle`
    /// without a register access. Called from `system.rs` at `FrameEnd`,
    /// next to the APU catch-up, so a chip's clock keeps ticking across a
    /// frame with no register traffic.
    pub fn catch_up_coprocessor(&mut self, master_cycle: u64) {
        if let Some(cart) = self.cartridge.as_mut() {
            cart.catch_up_chip(master_cycle);
        }
    }

    /// Whether the installed cartridge runs a burst-driven coprocessor CPU (the
    /// SA-1, the Super FX or the Cx4) that `system.rs` must catch up once per
    /// S-CPU instruction. `false` for plain / DSP-1 / S-DD1 / SPC7110 carts, so
    /// their cadence is unchanged.
    #[inline]
    pub fn cart_runs_burst_cpu(&self) -> bool {
        self.cart_runs_burst_cpu
    }

    /// Whether the installed cartridge is asserting a coprocessor IRQ toward the
    /// S-CPU (the SA-1 message / CC-DMA IRQ, the Super FX's at `stop`, the
    /// Cx4's when it goes idle). `system.rs` ORs this into the S-CPU IRQ line
    /// each instruction, gated by [`Self::cart_runs_burst_cpu`] so only a
    /// burst-CPU cart pays for the call. `false` with no cartridge.
    #[inline]
    pub fn cart_has_irq(&self) -> bool {
        self.cartridge.as_ref().is_some_and(|cart| cart.has_irq())
    }

    /// Reset the cartridge coprocessor (if any) and rebind its clock to
    /// `master_clock_hz`. Called from `SnesEmulator::reset` alongside
    /// [`Self::reset_apu`].
    pub fn reset_coprocessor(&mut self, master_clock_hz: u64) {
        if let Some(cart) = self.cartridge.as_mut() {
            cart.reset_chip(master_clock_hz);
        }
    }

    /// Choose what the PPU records while frames run ([`CaptureRecording`]),
    /// and keep the processor named as the writer of its stores while a live
    /// recorder needs it. The way to arm the recorders on a bus: arming them
    /// on the PPU alone leaves the processor's address unstamped.
    pub fn set_capture_recording(&mut self, recording: CaptureRecording) {
        self.b_bus.ppu_mut().set_capture_recording(recording);
        self.refresh_diag_on();
    }

    /// The debugger is about to write through the bus
    /// (`SnesEmulator::write_memory_debug`): the PPU puts what its ports
    /// take down to the debugger until [`Self::end_debugger_writes`].
    pub fn begin_debugger_writes(&mut self) {
        self.b_bus.ppu_mut().set_writer(WriteOrigin::DEBUGGER);
    }

    /// The debugger's writes are done: the processor writes again, from the
    /// instruction last stamped.
    pub fn end_debugger_writes(&mut self) {
        let pc = self.trace_pc;
        self.b_bus.ppu_mut().set_writer(WriteOrigin::processor(pc));
    }

    /// Reset the APU and rebind its rate converters to the console's
    /// region clock, clearing the cached catch-up cycle. Called from
    /// `SnesEmulator::reset`, which knows the active timing profile.
    pub fn reset_apu(&mut self, master_clock_hz: u64) {
        self.apu.reset(master_clock_hz);
        self.apu_cycle = 0;
    }

    /// Refresh the cached raster snapshot. The emulator's main
    /// loop calls this at the end of every CPU step with the
    /// scheduler's current `(h_dot, v_scanline)`, letting the
    /// PPU access gates and the `$2137` SLHV intercept observe
    /// up-to-1-instruction-stale raster state without the bus
    /// having to talk to the scheduler.
    pub fn set_raster_snapshot(&mut self, h_dot: u16, v_scanline: u16) {
        self.cached_h_dot = h_dot;
        self.cached_v_scanline = v_scanline;
    }

    /// Current cached H dot. Surfaced for tests and the debugger.
    pub fn cached_h_dot(&self) -> u16 {
        self.cached_h_dot
    }

    /// Current cached V scanline. Surfaced for tests and the
    /// debugger.
    pub fn cached_v_scanline(&self) -> u16 {
        self.cached_v_scanline
    }

    /// Build a fresh [`PpuAccessCtx`] from the current MDR and
    /// the cached raster snapshot. The B-bus dispatcher consumes
    /// this on every read/write to drive OAM/CGRAM active-scan
    /// gates and the STAT77 read.
    fn ppu_ctx(&self) -> PpuAccessCtx {
        PpuAccessCtx {
            mdr: self.mdr,
            h_dot: self.cached_h_dot,
            v_scanline: self.cached_v_scanline,
        }
    }

    /// Read-only borrow of the WRAM buffer.
    pub fn wram(&self) -> &[u8] {
        self.wram.as_ref()
    }

    /// Zero the 128 KiB work RAM. Used by the emulator's power-on (cold) reset:
    /// a real power cycle presents cleared WRAM, unlike the reset button (which
    /// preserves it). Netplay relies on this for a deterministic frame-0 baseline.
    pub fn clear_wram(&mut self) {
        self.wram.fill(0);
    }

    /// Inspect a single page-table entry.
    pub fn page_handler(&self, addr24: u32) -> PageHandler {
        self.page_table[(addr24 & 0x00FF_FFFF) as usize >> 12]
    }

    /// Master-cycle cost of a bus access at `addr24`.
    ///
    /// The system-area MMIO/RAM in the I/O banks (`$00-$3F` / `$80-$BF`,
    /// offset `< $8000`) has fixed, sub-page-accurate costs that the
    /// 4 KiB `speed_table` cannot represent, the `$4000` page straddles
    /// the 12 mclk manual-joypad window (`$4000-$41FF`) and the 6 mclk
    /// register file (`$4200-$5FFF`), and the low-RAM mirror
    /// (`$0000-$1FFF`, 8 mclk) sits in the same bank as the 6 mclk B-bus
    /// window (`$2000-$3FFF`). Those ranges are decoded directly here.
    ///
    /// Everything else, ROM (`$8000-$FFFF`), the WRAM banks
    /// (`$7E-$7F`), and the cart-only banks (`$40-$7F` / `$C0-$FF`),
    /// reads its [`build_default_speed_table`] entry, which the
    /// cartridge install path and [`SnesBus::apply_memsel`] keep current
    /// with the slot speed and the live FastROM/MEMSEL state.
    pub fn cycles_for(&self, addr24: u32) -> u8 {
        let addr = addr24 & 0x00FF_FFFF;
        let bank = (addr >> 16) as u8;
        let offset = (addr & 0xFFFF) as u16;
        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) && offset < 0x8000 {
            return match offset {
                // Low RAM / WRAM mirror.
                0x0000..=0x1FFF => BusSpeed::Slow.master_cycles(),
                // B-bus window: PPU, APU mailbox, WMDATA.
                0x2000..=0x3FFF => BusSpeed::Fast.master_cycles(),
                // Manual joypad ports + auto-read serial latches.
                0x4000..=0x41FF => BusSpeed::XSlow.master_cycles(),
                // CPU MMIO + DMA/HDMA register file (incl. $4212 HVBJOY,
                // $4218-$421F auto-joypad results).
                0x4200..=0x5FFF => BusSpeed::Fast.master_cycles(),
                // $6000-$7FFF expansion.
                _ => BusSpeed::Slow.master_cycles(),
            };
        }
        self.speed_table[(addr >> 12) as usize]
    }

    /// Install a cartridge.
    ///
    /// Walks the cartridge's [`MapPlan`] and, for every 4 KiB page
    /// each slot covers, promotes the page to
    /// [`PageHandler::Cartridge`] *only if the page is currently
    /// `OpenBus`*. This single rule does two things at once:
    ///
    /// 1. **Protects system pages.** WRAM, WRAM mirror, the B-bus
    ///    window, and CPU MMIO are never overwritten, so a slot
    ///    declared as `$00..$FF : $0000..$FFFF` cleanly maps only
    ///    the bytes that are not already owned by the 5A22.
    /// 2. **Implements first-wins between slots.** As soon as a
    ///    page is promoted it stops matching `OpenBus`, so later
    ///    slots covering the same page are silently ignored.
    ///    Consistent with [`crate::cartridge::MapPlan::resolve`],
    ///    which also picks the first slot in source order.
    ///
    /// The slot's [`crate::cartridge::BusSpeed`] is written into the
    /// parallel `speed_table` for every promoted page.
    ///
    /// The [`MapPlan`] is captured into an `Arc` so the bus can
    /// borrow slot metadata during dispatch without locking down the
    /// cartridge for write access, important because chip reads
    /// take `&mut self`.
    pub fn install_cartridge(&mut self, cartridge: Box<SnesCartridge>) {
        let plan = Arc::new(cartridge.map_plan().clone());
        for (slot_index, slot) in plan.slots.iter().enumerate() {
            let slot_index = slot_index as u16;
            let speed = slot.speed.master_cycles();
            for page in slot.covered_pages() {
                let entry = &mut self.page_table[page as usize];
                if matches!(*entry, PageHandler::OpenBus) {
                    *entry = PageHandler::Cartridge { slot_index };
                    self.speed_table[page as usize] = speed;
                }
            }
        }
        self.cartridge = Some(cartridge);
        self.cartridge_plan = Some(plan);
        // Gate the two expansion hooks once, so a plain cartridge never pays
        // for either on the hot MMIO path. A chip window that lands inside
        // the CpuMmio expansion page ($4800-$48FF) on a system-area bank
        // ($00-$3F/$80-$BF) arms the carve-out; a registered bus observer
        // (e.g. the S-DD1 spying on $43xx) arms the observe forward.
        self.cart_claims_expansion_mmio = self.cartridge_plan.as_ref().is_some_and(|plan| {
            plan.slots.iter().any(|s| {
                matches!(s.target, MapTarget::ChipMmio { .. })
                    && *s.offset_range.start() <= 0x48FF
                    && *s.offset_range.end() >= 0x4800
                    && (*s.bank_range.start() <= 0x3F
                        || (*s.bank_range.start() <= 0xBF && *s.bank_range.end() >= 0x80))
            })
        });
        // Same gate for the B-bus window: a chip MMIO slot overlapping
        // $2200-$23FF on a system-area bank ($00-$3F/$80-$BF) is the SA-1
        // register file, which the B-bus page can't promote either.
        self.cart_claims_bbus_mmio = self.cartridge_plan.as_ref().is_some_and(|plan| {
            plan.slots.iter().any(|s| {
                matches!(s.target, MapTarget::ChipMmio { .. })
                    && *s.offset_range.start() <= 0x23FF
                    && *s.offset_range.end() >= 0x2200
                    && (*s.bank_range.start() <= 0x3F
                        || (*s.bank_range.start() <= 0xBF && *s.bank_range.end() >= 0x80))
            })
        });
        // The Super FX is the burst chip that "opts in independently": its
        // register file lives at $3000-$34FF (nibble $3), a page the default
        // table leaves OpenBus and `install_cartridge` promotes to Cartridge,
        // so it needs no B-bus carve-out, only the per-instruction catch-up gate.
        // The SA-1 I-RAM slot ($3000-$37FF) overlaps this window too, but the
        // SA-1 already arms the flag via its B-bus claim, so the over-match is
        // a harmless no-op.
        let cart_claims_superfx_mmio = self.cartridge_plan.as_ref().is_some_and(|plan| {
            plan.slots.iter().any(|s| {
                matches!(s.target, MapTarget::ChipMmio { .. })
                    && *s.offset_range.start() <= 0x34FF
                    && *s.offset_range.end() >= 0x3000
                    && (*s.bank_range.start() <= 0x3F
                        || (*s.bank_range.start() <= 0xBF && *s.bank_range.end() >= 0x80))
            })
        });
        // The Cx4 is another burst chip that "opts in independently": its MMIO +
        // data-RAM window lives at $6000-$7FFF (nibble $6/$7), a page the default
        // table leaves OpenBus and `install_cartridge` promotes to Cartridge.
        // Matched on the Cx4 `chip_id` (not just the window) because the SPC7110
        // also maps write-gated SRAM as a `ChipMmio` slot over $6000-$7FFF, a
        // generic window match would false-positive it. The Cx4's own cart-RAM
        // slot ($70-77:0000-7FFF) shares the chip_id but fails the system-bank
        // check, so only the MMIO window arms the flag.
        let cart_claims_cx4_mmio = self.cartridge_plan.as_ref().is_some_and(|plan| {
            plan.slots.iter().any(|s| {
                matches!(
                    s.target,
                    MapTarget::ChipMmio {
                        chip_id: CX4_CHIP_ID,
                        ..
                    }
                ) && *s.offset_range.start() <= 0x7FFF
                    && *s.offset_range.end() >= 0x6000
                    && (*s.bank_range.start() <= 0x3F
                        || (*s.bank_range.start() <= 0xBF && *s.bank_range.end() >= 0x80))
            })
        });
        // The SA-1 (a B-bus MMIO claimer), the Super FX (an $3000-$34FF MMIO
        // claimer), and the Cx4 (a $6000-$7FFF MMIO claimer) each run a
        // burst-driven coprocessor CPU that must catch up per S-CPU instruction.
        // Keeping a distinct flag documents intent.
        self.cart_runs_burst_cpu =
            self.cart_claims_bbus_mmio || cart_claims_superfx_mmio || cart_claims_cx4_mmio;
        self.cart_observes_bus = self
            .cartridge
            .as_ref()
            .is_some_and(|cart| cart.observes_bus());
        // Cache the FastROM-eligible set, then pin the FastROM
        // region to Slow until the game enables MEMSEL. Matches the
        // SNES boot state (`$420D` bit 0 starts at 0).
        self.rebuild_fastrom_pages();
        self.apply_memsel(false);
    }

    /// Refresh the `fastrom_pages` cache from the currently
    /// installed cartridge plan. Called once during
    /// [`SnesBus::install_cartridge`]; future mid-emulation
    /// hot-swap of cartridges would call it again.
    ///
    /// The FastROM region is `$80-$BF:$8000-$FFFF` and the whole of
    /// banks `$C0-$FF`: the 5A22 charges an access there 6 master
    /// cycles while MEMSEL is set, 8 otherwise. A HiROM game running
    /// from `$C0-$FF` is as fast as one running from `$80-$BF`.
    fn rebuild_fastrom_pages(&mut self) {
        self.fastrom_pages.clear();
        let Some(plan) = self.cartridge_plan.as_ref() else {
            return;
        };
        for slot in &plan.slots {
            if slot.speed != BusSpeed::Fast {
                continue;
            }
            for page in slot.covered_pages() {
                let bank = page >> 4;
                let offset_nibble = page & 0xF;
                if ((0x80..=0xBF).contains(&bank) && (0x8..=0xF).contains(&offset_nibble))
                    || (0xC0..=0xFF).contains(&bank)
                {
                    self.fastrom_pages.push(page);
                }
            }
        }
    }

    /// Switch every FastROM-eligible page between Slow (8) and Fast
    /// (6) master cycles. Called by [`SnesBus::install_cartridge`]
    /// (pinning to Slow at boot) and by the `$420D` write handler.
    ///
    /// Pages of the FastROM region whose slot was declared
    /// `BusSpeed::Slow` or `XSlow` are NOT touched, those cartridges
    /// don't have FastROM hardware, so MEMSEL is a no-op for them.
    pub fn apply_memsel(&mut self, fast: bool) {
        self.memsel_fast = fast;
        let speed = if fast { 6 } else { 8 };
        for &page in &self.fastrom_pages {
            if matches!(
                self.page_table[page as usize],
                PageHandler::Cartridge { .. }
            ) {
                self.speed_table[page as usize] = speed;
            }
        }
    }

    /// Borrow the installed cartridge, if any.
    pub fn cartridge(&self) -> Option<&SnesCartridge> {
        self.cartridge.as_deref()
    }

    /// Mutable borrow of the installed cartridge, if any.
    pub fn cartridge_mut(&mut self) -> Option<&mut SnesCartridge> {
        self.cartridge.as_deref_mut()
    }

    /// The installed cartridge's [`MapPlan`], or `None` when no
    /// cartridge is installed. Used by the cartridge debugger to render
    /// the memory map and ROM/SRAM regions.
    pub fn cartridge_plan(&self) -> Option<&MapPlan> {
        self.cartridge_plan.as_deref()
    }

    /// What an A-bus address reads, through the page table and the
    /// cartridge's map plan as they stand, the way a read of it is routed:
    /// work RAM (`$7E-$7F`, and the low 8 KiB of a system bank), a byte of
    /// the cartridge's ROM image (the image the cartridge holds: the copier
    /// header stripped, padded to a power of two; nothing past it), the
    /// cartridge's RAM, a coprocessor's window, or nothing it can name.
    /// Pure: it reads the mapping and changes nothing.
    ///
    /// A coprocessor's window answers as its chip says
    /// ([`Coprocessor::source_place`](retro_core::cartridge::Coprocessor::source_place)):
    /// the Super FX's and the Cx4's ROM windows reach the image with the
    /// plain ROM geometry, so they name the image's byte; the SA-1's, the
    /// S-DD1's and the SPC7110's ROM windows go through the chip's own
    /// banks, so they name the coprocessor. The Super FX's and the Cx4's
    /// cartridge RAM, the SA-1's BW-RAM and the SPC7110's SRAM name the
    /// cartridge's RAM.
    pub fn source_place(&self, address: u32) -> SourcePlace {
        let addr24 = address & 0x00FF_FFFF;
        let low = addr24 & 0xFFFF;
        let plan_place = || match (self.cartridge_plan.as_deref(), self.cartridge.as_deref()) {
            (Some(plan), Some(cart)) => {
                plan.resolve(addr24)
                    .map_or(SourcePlace::Unknown, |resolved| {
                        let slot = &plan.slots[resolved.slot_index as usize];
                        cart.target_source_place(slot.target, resolved.target_offset)
                    })
            }
            _ => SourcePlace::Unknown,
        };
        match self.page_table[(addr24 >> 12) as usize] {
            PageHandler::Wram | PageHandler::WramMirror => SourcePlace::WorkRam,
            PageHandler::Cartridge { slot_index } => {
                let (Some(plan), Some(cart)) =
                    (self.cartridge_plan.as_deref(), self.cartridge.as_deref())
                else {
                    return SourcePlace::Unknown;
                };
                match plan.slots.get(usize::from(slot_index)) {
                    Some(slot) if slot.contains(addr24) => {
                        cart.target_source_place(slot.target, slot.compute_target_offset(addr24))
                    }
                    _ => SourcePlace::Unknown,
                }
            }
            PageHandler::BBus if self.cart_claims_bbus_mmio && (0x2200..=0x23FF).contains(&low) => {
                plan_place()
            }
            PageHandler::CpuMmio
                if self.cart_claims_expansion_mmio && (0x4800..=0x48FF).contains(&low) =>
            {
                plan_place()
            }
            _ => SourcePlace::Unknown,
        }
    }

    /// The byte a read of an A-bus address would give, read without side
    /// effects, for the debugger's own reads: the memory viewer, the
    /// disassembly, the trace's operands and pointers, the call stack's
    /// opcode and a condition's memory reads.
    ///
    /// Work RAM and its mirror give their bytes, and the cartridge's ROM
    /// and RAM theirs, routed through the page table and the map plan as a
    /// read is. A coprocessor's window answers through the chip's own peek
    /// ([`Cartridge::peek_chip`]), which gives the ROM or RAM behind it
    /// and nothing for a register; where the chip cannot answer, and on an
    /// unmapped page, the peek gives the open-bus value, as a read of
    /// nothing would. The B-bus and CPU register pages read 0, because a
    /// read of a register can change it (a latch, a flag cleared by its
    /// read, a port that steps its address). A peek counts no cycle, moves
    /// no latch, records no access and catches no coprocessor up.
    pub fn peek(&self, address: u32) -> u8 {
        let addr24 = address & 0x00FF_FFFF;
        let low = addr24 & 0xFFFF;
        match self.page_table[(addr24 >> 12) as usize] {
            PageHandler::Wram => self.wram[(addr24 - 0x7E_0000) as usize],
            PageHandler::WramMirror => self.wram[(addr24 & 0x1FFF) as usize],
            PageHandler::Cartridge { slot_index } => self.peek_slot(slot_index, addr24),
            PageHandler::BBus if self.cart_claims_bbus_mmio && (0x2200..=0x23FF).contains(&low) => {
                self.peek_planned(addr24)
            }
            PageHandler::CpuMmio
                if self.cart_claims_expansion_mmio && (0x4800..=0x48FF).contains(&low) =>
            {
                self.peek_planned(addr24)
            }
            PageHandler::BBus | PageHandler::CpuMmio => 0,
            PageHandler::OpenBus => self.mdr,
        }
    }

    /// [`SnesBus::peek`] of a chip window carved out of a register page,
    /// found through the map plan as the read path finds it.
    fn peek_planned(&self, addr24: u32) -> u8 {
        match self
            .cartridge_plan
            .as_deref()
            .and_then(|plan| plan.resolve(addr24))
        {
            Some(resolved) => self.peek_slot(resolved.slot_index, addr24),
            None => self.mdr,
        }
    }

    /// [`SnesBus::peek`] of an address the map plan's slot `slot_index`
    /// covers.
    fn peek_slot(&self, slot_index: u16, addr24: u32) -> u8 {
        let (Some(plan), Some(cart)) = (self.cartridge_plan.as_deref(), self.cartridge.as_deref())
        else {
            return self.mdr;
        };
        let Some(slot) = plan.slots.get(usize::from(slot_index)) else {
            return self.mdr;
        };
        if !slot.contains(addr24) {
            return self.mdr;
        }
        cart.peek_target(slot.target, slot.compute_target_offset(addr24), self.mdr)
    }

    /// Battery SRAM bytes of the installed cartridge, or `None` when no
    /// cartridge is installed or it carries no SRAM. Backs
    /// `EmulatorCore::get_save_ram`.
    pub fn cartridge_sram(&self) -> Option<&[u8]> {
        let sram = self.cartridge.as_ref()?.sram();
        if sram.is_empty() {
            None
        } else {
            Some(sram)
        }
    }

    /// Overwrite the installed cartridge's battery SRAM from `data`
    /// (size-checked by the cartridge). No-op when no cartridge is
    /// installed. Backs `EmulatorCore::set_save_ram` and the savestate
    /// SRAM restore.
    pub fn restore_cartridge_sram(&mut self, data: &[u8]) {
        if let Some(cart) = self.cartridge.as_mut() {
            cart.restore_sram(data);
        }
    }

    /// Coprocessor chip states of the installed cartridge, keyed by
    /// `chip_id`. Empty when no cartridge is installed or it has no chips.
    /// Each entry is backend-tagged (LLE vs HLE). Backs the savestate
    /// capture (mirrors [`SnesBus::cartridge_sram`]).
    pub fn coprocessors_savestate(&self) -> Vec<(u8, crate::savestate::SnesChipState)> {
        self.cartridge
            .as_ref()
            .map(|c| c.coprocessors_savestate())
            .unwrap_or_default()
    }

    /// Restore the installed cartridge's coprocessor chip states, rebinding
    /// each lazy-catch-up converter to `master_clock_hz`. No-op when no
    /// cartridge is installed or it carries no chips (mirrors
    /// [`SnesBus::restore_cartridge_sram`]).
    pub fn restore_coprocessors_savestate(
        &mut self,
        states: &[(u8, crate::savestate::SnesChipState)],
        master_clock_hz: u64,
    ) {
        if let Some(cart) = self.cartridge.as_mut() {
            cart.restore_coprocessors_savestate(states, master_clock_hz);
        }
    }

    /// Capture every volatile field the bus owns, WRAM, the MDR, the
    /// MEMSEL bit, the cached raster snapshot, the deferred pending
    /// flags, the APU catch-up cycle, every owned subsystem, and the
    /// cartridge's battery SRAM, into a snapshot. The page/speed
    /// tables, the cartridge, and its map plan are *not* captured: the
    /// ROM is already loaded when a restore runs.
    pub fn to_savestate(&self) -> crate::savestate::BusState {
        crate::savestate::BusState {
            wram: self.wram.to_vec(),
            mdr: self.mdr,
            memsel_fast: self.memsel_fast,
            cached_h_dot: self.cached_h_dot,
            cached_v_scanline: self.cached_v_scanline,
            pending_general_dma: self.pending_general_dma,
            pending_irq_reschedule: self.pending_irq_reschedule,
            pending_nmi: self.pending_nmi,
            apu_cycle: self.apu_cycle,
            ppu: self.b_bus.ppu().to_savestate(),
            apu: self.apu.to_savestate(),
            dma: self.dma.to_savestate(),
            system_regs: self.system_regs.to_savestate(),
            controllers: [self.controllers[0].state(), self.controllers[1].state()],
            wmdata: self.wmdata.to_savestate(),
            sram: self
                .cartridge_sram()
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            coprocessors: self.coprocessors_savestate(),
        }
    }

    /// Restore every volatile field from a snapshot, leaving the
    /// cartridge / map plan / page+speed tables intact (the ROM is
    /// already loaded). `master_clock_hz` is the region clock the APU
    /// rate converters re-derive their ratio from. FastROM speeds are
    /// re-synced via [`SnesBus::apply_memsel`] from the restored MEMSEL
    /// bit, and the battery SRAM is written back into the live
    /// cartridge.
    pub fn from_savestate(&mut self, s: &crate::savestate::BusState, master_clock_hz: u64) {
        self.wram.copy_from_slice(&s.wram);
        self.mdr = s.mdr;
        self.cached_h_dot = s.cached_h_dot;
        self.cached_v_scanline = s.cached_v_scanline;
        self.pending_general_dma = s.pending_general_dma;
        self.pending_irq_reschedule = s.pending_irq_reschedule;
        self.pending_nmi = s.pending_nmi;
        self.apu_cycle = s.apu_cycle;
        self.b_bus.ppu_mut().from_savestate(&s.ppu);
        self.apu.from_savestate(&s.apu, master_clock_hz);
        self.dma.from_savestate(&s.dma);
        self.system_regs.from_savestate(&s.system_regs);
        self.controllers[0].restore(&s.controllers[0]);
        self.controllers[1].restore(&s.controllers[1]);
        self.wmdata.from_savestate(&s.wmdata);
        self.restore_cartridge_sram(&s.sram);
        // Restore every coprocessor (uPD7725 + converter anchor, ...) the
        // state carries into the chip the live cartridge hosts at that id.
        // The firmware is already installed (staged before `load_rom`),
        // exactly like the ROM image, so only the volatile chip state is
        // restored.
        if !s.coprocessors.is_empty() {
            self.restore_coprocessors_savestate(&s.coprocessors, master_clock_hz);
        }
        // Re-derive the FastROM page speeds from the restored MEMSEL
        // bit. `apply_memsel` also sets `self.memsel_fast`, so the hot
        // path mirror and the speed table agree again.
        self.apply_memsel(s.memsel_fast);
    }

    /// A B-bus read for DMA that bypasses A-bus decode, after the
    /// parallel A/B strobing on the real hardware (the A-side may be
    /// carrying an unrelated fetch in the same master cycle). Does
    /// **not** touch MDR. It is a stub that returns 0, and nothing
    /// calls it: the DMA engine moves its bytes through the bus's
    /// dispatch at `$2100 | port`.
    pub fn b_bus_dma_read(&mut self, reg_offset: u8) -> u8 {
        self.b_bus.dma_read(reg_offset)
    }

    /// DMA write counterpart to [`SnesBus::b_bus_dma_read`].
    pub fn b_bus_dma_write(&mut self, reg_offset: u8, value: u8) {
        self.b_bus.dma_write(reg_offset, value);
    }

    fn read_b_bus_window(&mut self, addr24: u32) -> u8 {
        let lo = (addr24 & 0xFFFF) as u16;
        match lo {
            // WMDATA: 5A22-side WRAM portal, lives on the bus
            // itself rather than on the B-bus dispatcher.
            0x2180 => self.wmdata.read_data(self.wram.as_ref()),
            // WMADDL / WMADDH / WMADDB are write-only; reads
            // surface the MDR latch on real hardware.
            0x2181..=0x2183 => self.mdr,
            // SLHV: reading $2137 latches the current H/V counters
            // into OPHCT/OPVCT and arms the `counters_latched`
            // flag the STAT78 read consumes. The byte returned
            // is open-bus (MDR); checked against bsnes-plus.
            0x2137 => {
                self.system_regs
                    .latch_hv(self.cached_h_dot, self.cached_v_scanline);
                self.mdr
            }
            // OPHCT / OPVCT: H/V counter latches, driven by the
            // SystemRegs object the bus owns. The B-bus
            // dispatcher itself does not see these reads.
            0x213C => self.system_regs.read_hcounter(),
            0x213D => self.system_regs.read_vcounter(),
            // STAT78: PPU2 status. Build the byte locally, the
            // BBus PPU never sees this address. Its bits (checked
            // against bsnes-plus):
            //
            // - bit 7: interlace field, the PPU toggles its
            //   internal `field` once per frame at VBlankStart
            //   regardless of whether interlace is enabled, so
            //   the bit alternates the way real silicon does.
            // - bit 6: `!counters_latched`. The "external latch
            //   flag" reads 1 when nothing recent has latched HV
            //   (idle), 0 just after `latch_hv`. Reading $213F
            //   also clears the underlying flag.
            // - bit 5: open bus from PPU2 MDR's bit 5.
            // - bit 4: PAL (1) vs NTSC (0).
            // - bits 0-3: PPU2 version.
            //
            // Also resets the H/V toggle states so the next
            // OPHCT/OPVCT read pair starts on the low byte.
            0x213F => {
                let counters_latched = self.system_regs.consume_counters_latched_flag();
                let pal = self.system_regs.timing_is_pal();
                let field = self.b_bus.ppu().field();
                self.system_regs.reset_hv_toggles();
                let ppu2_mdr = self.b_bus.ppu().ppu2_mdr();
                let mut r = ppu2_mdr & 0x20;
                if field {
                    r |= 0x80;
                }
                if !counters_latched {
                    r |= 0x40;
                }
                if pal {
                    r |= 0x10;
                }
                r |= PPU2_VERSION & 0x0F;
                // Update PPU2 MDR with the byte we just drove.
                self.b_bus.ppu_mut().set_ppu2_mdr(r);
                r
            }
            // APU mailbox: ports 0-3 at $2140-$2143, mirrored every 4
            // bytes through $217F. The read catches the SPC700 up to
            // the current master cycle so it observes the SPC's latest
            // write to that port.
            0x2140..=0x217F => {
                let cyc = self.apu_access_cycle();
                self.apu.read_port((lo & 0x03) as u8, cyc)
            }
            0x2100..=0x21FF => {
                let reg = (lo - 0x2100) as u8;
                self.b_bus.read(reg, self.ppu_ctx())
            }
            // SA-1 register file $2200-$23FF. The page-table walker can't
            // promote this (the whole $2000 page is the BBus window), so a
            // cartridge that maps a chip window here (armed once at install)
            // gets the access resolved through the plan, the ChipMmio dispatch
            // + coprocessor trace then behave exactly like a chip read
            // anywhere else. Gated so plain carts never reach it.
            0x2200..=0x23FF if self.cart_claims_bbus_mmio => {
                let slot = self
                    .cartridge_plan
                    .as_deref()
                    .and_then(|plan| plan.resolve(addr24))
                    .map(|resolved| resolved.slot_index);
                match slot {
                    Some(index) => self.read_cartridge(index, addr24),
                    None => self.mdr,
                }
            }
            // Bytes inside the BBus page but outside the 256-byte
            // window are open bus.
            _ => self.mdr,
        }
    }

    fn write_b_bus_window(&mut self, addr24: u32, value: u8) {
        let lo = (addr24 & 0xFFFF) as u16;
        match lo {
            0x2180 => self.wmdata.write_data(self.wram.as_mut(), value),
            0x2181 => self.wmdata.write_addr_low(value),
            0x2182 => self.wmdata.write_addr_high(value),
            0x2183 => self.wmdata.write_addr_bank(value),
            // APU mailbox: see `read_b_bus_window`. The write catches
            // the SPC700 up first so it is ordered correctly against
            // the SPC's own port traffic.
            0x2140..=0x217F => {
                let cyc = self.apu_access_cycle();
                self.apu.write_port((lo & 0x03) as u8, value, cyc)
            }
            0x2100..=0x21FF => {
                let reg = (lo - 0x2100) as u8;
                self.b_bus.write(reg, value, self.ppu_ctx());
            }
            // SA-1 register file $2200-$23FF (write side of the read carve-out
            // above): resolve through the map plan and reuse `write_cartridge`
            // for the ChipMmio dispatch. Gated so plain carts never reach it.
            0x2200..=0x23FF if self.cart_claims_bbus_mmio => {
                let slot = self
                    .cartridge_plan
                    .as_deref()
                    .and_then(|plan| plan.resolve(addr24))
                    .map(|resolved| resolved.slot_index);
                if let Some(index) = slot {
                    self.write_cartridge(index, addr24, value);
                }
            }
            _ => {}
        }
    }

    fn read_cpu_mmio(&mut self, addr24: u32) -> u8 {
        let lo = (addr24 & 0xFFFF) as u16;
        match lo {
            // HVBJOY HBlank flag (bit 6) is a pure function of the dot
            // position, so compute it live from the cached raster rather
            // than trusting the stored byte. The scheduler only toggles
            // the stored bit 6 at dot 274 (HBlankStart) and dot 22
            // (RenderPos), which leaves it set across the whole
            // [0, 22) start-of-line window and never clears it on the
            // RenderPos-less line 0, so a "wait for HBlank to begin"
            // poll would latch the H counter at the start of the line
            // instead of dot 274 (the Electronics Test of Nintendo's SNES
            // Test Program checks this).
            // HBlank is the stretch from dot 274 through dot 0 of the
            // next line (checked against snes9x). Bits 7 (VBlank) and 0
            // (auto-joypad busy) stay event-driven.
            0x4212 => {
                let stored = self.system_regs.read(0x4212) & !0x40;
                let h = self.cached_h_dot;
                let hblank = h == 0 || h >= 274;
                stored | if hblank { 0x40 } else { 0 }
            }
            // System registers $4210-$421F (read-side surface).
            // TIMEUP. An H/V IRQ that rose inside this instruction, before
            // this read, is visible already: the read returns bit 7 and
            // acknowledges the IRQ (see `set_next_irq_cycle`).
            0x4211 => {
                let value = self.system_regs.read(lo);
                match self.next_irq_cycle {
                    Some(at) if at <= self.timeup_access_cycle() => {
                        self.next_irq_cycle = None;
                        self.irq_read_early = Some(at);
                        value | 0x80
                    }
                    _ => value,
                }
            }
            0x4210..=0x421F => self.system_regs.read(lo),
            // $4200-$420F are write-only, reads return MDR.
            0x4200..=0x420F => self.mdr,
            // $4016/$4017: manual joypad serial read. Bit 0 is the
            // controller data line (port 0 on $4016, port 1 on $4017);
            // the upper bits are open bus, so preserve MDR there.
            0x4016 => (self.mdr & 0xFE) | (self.controllers[0].read_serial() & 1),
            0x4017 => (self.mdr & 0xFE) | (self.controllers[1].read_serial() & 1),
            // $4300-$437F: DMA channel registers. The low nibble
            // selects the field; channel = bits 4-6. Unmapped slots
            // ($43xC-E) read open bus (MDR).
            0x4300..=0x437F => {
                let channel = ((lo >> 4) & 0x7) as usize;
                let offset = (lo & 0xF) as u8;
                self.dma.read_reg(channel, offset).unwrap_or(self.mdr)
            }
            // Expansion-chip MMIO carved out of the CpuMmio page: a chip
            // window at $4800-$48FF (S-DD1, SPC7110) the page-table walker
            // can't promote (the whole $4000 page is CpuMmio). Resolve it
            // through the cartridge map plan and reuse `read_cartridge` so
            // the ChipMmio dispatch + coprocessor trace behave exactly like
            // a chip read anywhere else. Gated so plain carts never reach it.
            0x4800..=0x48FF if self.cart_claims_expansion_mmio => {
                let slot = self
                    .cartridge_plan
                    .as_deref()
                    .and_then(|plan| plan.resolve(addr24))
                    .map(|resolved| resolved.slot_index);
                match slot {
                    Some(index) => self.read_cartridge(index, addr24),
                    None => self.mdr,
                }
            }
            _ => self.mdr,
        }
    }

    fn write_cpu_mmio(&mut self, addr24: u32, value: u8) {
        let lo = (addr24 & 0xFFFF) as u16;
        match lo {
            // System registers $4200-$420F (write-side surface).
            // Side-effects (MEMSEL rebuild, HV latch on WRIO
            // falling edge) come back as a SystemRegWriteFx that
            // the bus interprets here, keeping SystemRegs free of
            // any bus-shaped knowledge.
            0x4200..=0x420F => match self.system_regs.write(lo, value) {
                SystemRegWriteFx::None => {}
                SystemRegWriteFx::LatchHv => {
                    // WRIO ($4201) bit 7 falling edge latches HV
                    // from the cached raster snapshot. The cache
                    // is refreshed at the end of every CPU step,
                    // so the value is at most one instruction
                    // stale, accurate enough for the read-back
                    // surface OPHCT/OPVCT expose.
                    self.system_regs
                        .latch_hv(self.cached_h_dot, self.cached_v_scanline);
                }
                SystemRegWriteFx::MemselChanged(fast) => self.apply_memsel(fast),
                SystemRegWriteFx::GeneralDmaRequested(mask) => {
                    // Defer the transfer to the instruction boundary
                    // (as snes9x does too): running it from inside a bus
                    // write would re-enter the bus while the CPU is
                    // mid-store. `system.rs` drains this after the
                    // instruction and folds the stolen cycles into
                    // `master_cycles`.
                    self.pending_general_dma = mask;
                }
                SystemRegWriteFx::IrqConfigChanged { retrigger_nmi } => {
                    // Defer the reschedule to the instruction boundary
                    // (same model as general DMA). `system.rs` cancels
                    // the stale H/V IRQ events and re-arms them from
                    // the current frame base. The NMI re-trigger edge
                    // also routes through a pending flag because the
                    // bus cannot reach the CPU directly.
                    self.pending_irq_reschedule = true;
                    if retrigger_nmi {
                        self.pending_nmi = true;
                    }
                }
            },
            // $4016 bit 0 is the shared latch (strobe) line for BOTH
            // controller ports; the high-to-low edge reloads each pad's
            // serial shift register. $4017 is read-only.
            0x4016 => {
                let high = (value & 1) != 0;
                for pad in &mut self.controllers {
                    pad.strobe(high);
                }
            }
            0x4017 => {}
            // $4300-$437F: DMA channel registers. The low nibble
            // selects the field; channel = bits 4-6. Unmapped slots
            // ($43xC-E) drop the write.
            0x4300..=0x437F => {
                let channel = ((lo >> 4) & 0x7) as usize;
                let offset = (lo & 0xF) as u8;
                self.dma.write_reg(channel, offset, value);
                // Passive observation: after the DMA controller consumes the
                // write, hand the same byte to a cartridge chip that spies on
                // the DMA registers (the S-DD1 reconstructs each channel to
                // decompress during transfer). `lo` is the bank-agnostic
                // 16-bit register offset, so the chip's single observed
                // range ($4300-$437F) matches the $00-$3F *and* $80-$BF
                // mirrors of the CpuMmio page. The DMA engine is untouched.
                if self.cart_observes_bus {
                    if let Some(cart) = self.cartridge.as_mut() {
                        cart.observe_bus_write(lo as u32, value);
                    }
                }
            }
            // $4210-$421F are read-only on real hardware: writes
            // are dropped.
            0x4210..=0x421F => {}
            // Expansion-chip MMIO carved out of the CpuMmio page (write side
            // of the $4800-$48FF read carve-out): resolve through the map
            // plan and reuse `write_cartridge` for the ChipMmio dispatch.
            0x4800..=0x48FF if self.cart_claims_expansion_mmio => {
                let slot = self
                    .cartridge_plan
                    .as_deref()
                    .and_then(|plan| plan.resolve(addr24))
                    .map(|resolved| resolved.slot_index);
                if let Some(index) = slot {
                    self.write_cartridge(index, addr24, value);
                }
            }
            _ => {}
        }
    }

    fn read_cartridge(&mut self, slot_index: u16, addr24: u32) -> u8 {
        let Some(plan) = self.cartridge_plan.clone() else {
            return self.mdr;
        };
        let Some(slot) = plan.slots.get(slot_index as usize) else {
            return self.mdr;
        };
        if !slot.contains(addr24) {
            return self.mdr;
        }
        let target_offset = slot.compute_target_offset(addr24);
        let mdr = self.mdr;
        let chip_cycle = self.chip_access_cycle();
        let target = slot.target;
        let value = match self.cartridge.as_mut() {
            Some(cart) => cart.read_target(target, target_offset, chip_cycle, mdr),
            None => return mdr,
        };
        if self.coprocessor_trace.is_some() {
            if let MapTarget::ChipMmio { base, .. } = target {
                if let Some(log) = self.coprocessor_trace.as_mut() {
                    log.push((false, base + target_offset, value));
                }
            }
        }
        value
    }

    fn write_cartridge(&mut self, slot_index: u16, addr24: u32, value: u8) {
        let Some(plan) = self.cartridge_plan.clone() else {
            return;
        };
        let Some(slot) = plan.slots.get(slot_index as usize) else {
            return;
        };
        if !slot.contains(addr24) {
            return;
        }
        let target_offset = slot.compute_target_offset(addr24);
        let chip_cycle = self.chip_access_cycle();
        let target = slot.target;
        // Cartridge ROM is read-only; SNES ROM "writes" are how some chips
        // (SA-1, SuperFX) signal their MMIO via the ChipMmio target rather
        // than a Rom write.
        match self.cartridge.as_mut() {
            Some(cart) => cart.write_target(target, target_offset, value, chip_cycle),
            None => return,
        }
        if self.coprocessor_trace.is_some() {
            if let MapTarget::ChipMmio { base, .. } = target {
                if let Some(log) = self.coprocessor_trace.as_mut() {
                    log.push((true, base + target_offset, value));
                }
            }
        }
    }

    /// Core read dispatch. Two indexed loads (page-table entry
    /// plus the dispatch target) and one branch, the same shape
    /// every tier shares. MDR is updated to the returned value.
    #[inline]
    fn read_inner(&mut self, addr: u32) -> u8 {
        let addr24 = addr & 0x00FF_FFFF;
        let handler = self.page_table[(addr24 >> 12) as usize];
        let v = match handler {
            PageHandler::Wram => self.wram[(addr24 - 0x7E_0000) as usize],
            PageHandler::WramMirror => self.wram[(addr24 & 0x1FFF) as usize],
            PageHandler::BBus => self.read_b_bus_window(addr24),
            PageHandler::CpuMmio => self.read_cpu_mmio(addr24),
            PageHandler::Cartridge { slot_index } => self.read_cartridge(slot_index, addr24),
            PageHandler::OpenBus => self.mdr,
        };
        self.mdr = v;
        v
    }

    /// Core write dispatch. MDR is updated *before* the dispatch
    /// so an open-bus write still leaves the value latched. With `NOTE`, a
    /// write to one of the PPU's three data ports goes through the path that
    /// notes what it takes ([`Ppu::write_port_noted`]); every other write,
    /// and every write without `NOTE`, is dispatched as it always was. The
    /// caller chooses once, per write of the processor or per DMA block and
    /// HDMA line (see [`Self::write_full`]).
    #[inline]
    fn write_inner<const NOTE: bool>(&mut self, addr: u32, value: u8) {
        let addr24 = addr & 0x00FF_FFFF;
        self.mdr = value;
        match self.page_table[(addr24 >> 12) as usize] {
            PageHandler::Wram => self.wram[(addr24 - 0x7E_0000) as usize] = value,
            PageHandler::WramMirror => self.wram[(addr24 & 0x1FFF) as usize] = value,
            PageHandler::BBus if NOTE && is_noted_port(addr24) => {
                let ctx = self.ppu_ctx();
                self.b_bus
                    .ppu_mut()
                    .write_port_noted((addr24 & 0x3F) as u8, value, &ctx);
            }
            PageHandler::BBus => self.write_b_bus_window(addr24, value),
            PageHandler::CpuMmio => self.write_cpu_mmio(addr24, value),
            PageHandler::Cartridge { slot_index } => {
                self.write_cartridge(slot_index, addr24, value)
            }
            PageHandler::OpenBus => { /* no-op: MDR already latched above */ }
        }
    }

    /// Fastest read tier (`ExecutionTier::Play`): pure dispatch,
    /// no instrumentation. Used when the CPU step loop runs free.
    #[inline(always)]
    pub fn read_fast(&mut self, addr: u32) -> u8 {
        self.read_inner(addr)
    }

    /// Light-instrumentation read tier
    /// (`ExecutionTier::LightDebug`). Delegates to `read_inner`; the
    /// debugger records coverage per instruction, not per access.
    pub fn read_light(&mut self, addr: u32) -> u8 {
        self.read_inner(addr)
    }

    /// Full-instrumentation read tier (`ExecutionTier::FullDebug`).
    /// Delegates to `read_inner`; watchpoint events and the diagnostic
    /// traces are recorded around it, in the [`Addressable`] impl.
    pub fn read_full(&mut self, addr: u32) -> u8 {
        self.read_inner(addr)
    }

    /// A write of the processor, or of the debugger through the bus: the
    /// one test of whether the PPU notes what its ports take
    /// ([`Ppu::notes_port_writes`]), then the dispatch.
    #[inline]
    fn write_tested(&mut self, addr: u32, value: u8) {
        if self.b_bus.ppu().notes_port_writes() {
            self.write_inner::<true>(addr, value);
        } else {
            self.write_inner::<false>(addr, value);
        }
    }

    /// Fastest write tier. Mirror of [`SnesBus::read_fast`].
    #[inline(always)]
    pub fn write_fast(&mut self, addr: u32, value: u8) {
        self.write_tested(addr, value);
    }

    /// Light-instrumentation write tier. Mirror of
    /// [`SnesBus::read_light`].
    pub fn write_light(&mut self, addr: u32, value: u8) {
        self.write_tested(addr, value);
    }

    /// Full-instrumentation write tier. Mirror of
    /// [`SnesBus::read_full`].
    pub fn write_full(&mut self, addr: u32, value: u8) {
        self.write_tested(addr, value);
    }
}

/// The bus as the debugger reads it: an [`Addressable`] over
/// [`SnesBus::peek`], for the disassembler, the trace's address
/// resolution and the call stack's opcode read. Reads change nothing;
/// writes are dropped.
pub struct SnesPeekBus<'a>(pub &'a SnesBus);

impl Addressable for SnesPeekBus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        self.0.peek(addr)
    }

    fn write(&mut self, _addr: u32, _value: u8) {}
}

/// Build the default page table: WRAM (`$7E-$7F`), WRAM mirror in
/// the low half of every system-area bank, the B-bus window page,
/// the CPU MMIO page, and open bus everywhere else. Cartridge pages
/// are filled in by [`SnesBus::install_cartridge`].
pub fn build_default_page_table() -> [PageHandler; PAGE_TABLE_LEN] {
    let mut table = [PageHandler::OpenBus; PAGE_TABLE_LEN];
    for (page, entry) in table.iter_mut().enumerate() {
        let bank = (page >> 4) as u8;
        let high_nibble = (page & 0xF) as u16;
        *entry = match bank {
            0x7E..=0x7F => PageHandler::Wram,
            0x00..=0x3F | 0x80..=0xBF => match high_nibble {
                // $0000-$0FFF + $1000-$1FFF: first 8 KiB of WRAM
                // mirrored into every system-area bank.
                0x0 | 0x1 => PageHandler::WramMirror,
                // $2000-$2FFF: page that contains the B-bus window
                // at $2100-$21FF. Bytes outside that 256-byte range
                // still land here but get filtered to MDR inside
                // the dispatcher.
                0x2 => PageHandler::BBus,
                // $4000-$4FFF: page that contains the manual joypad
                // registers and the CPU MMIO range $4200-$43FF.
                0x4 => PageHandler::CpuMmio,
                // Everything else in the system area is either
                // cartridge territory ($8000-$FFFF in $00-$3F, the
                // FastROM area in $80-$BF) or genuinely open bus
                // ($3000-$3FFF, $5000-$7FFF). install_cartridge()
                // overwrites cartridge pages after the fact.
                _ => PageHandler::OpenBus,
            },
            // Cart-only banks: $40-$7D and $C0-$FF. Filled in by
            // install_cartridge() through the slot walker.
            _ => PageHandler::OpenBus,
        };
    }
    table
}

/// Build the default speed table.
///
/// Seeds every page with Slow (8), the SNES default for ROM and RAM.
/// The sub-page-accurate system-area MMIO/RAM costs (`$0000-$7FFF` in
/// the I/O banks) are decoded directly by [`SnesBus::cycles_for`], which
/// short-circuits this table for that range, a 4 KiB page is too coarse
/// to split the 12 mclk joypad window from the 6 mclk register file in
/// the `$4000` page, or the 8 mclk low RAM from the 6 mclk B-bus window.
/// This table is therefore authoritative only for ROM (`$8000-$FFFF`),
/// the WRAM banks (`$7E-$7F`), and the cart-only banks; cartridge pages
/// are overwritten with `slot.speed.master_cycles()` by
/// [`SnesBus::install_cartridge`] and retuned by
/// [`SnesBus::apply_memsel`] when FastROM toggles.
pub fn build_default_speed_table() -> [u8; PAGE_TABLE_LEN] {
    [BusSpeed::Slow.master_cycles(); PAGE_TABLE_LEN]
}

impl Addressable for SnesBus {
    /// Delegates to [`SnesBus::read_full`] (the safe default).
    /// Code paths that know which tier they want call the typed
    /// variants directly.
    fn read(&mut self, addr: u32) -> u8 {
        self.instr_master_cycles += self.cycles_for(addr) as u64;
        self.instr_accesses += 1;
        let value = self.read_full(addr);
        if self.records_accesses() {
            self.record_access(false, addr, value);
        }
        value
    }

    /// Delegates to [`SnesBus::write_full`] (the safe default).
    fn write(&mut self, addr: u32, value: u8) {
        self.instr_master_cycles += self.cycles_for(addr) as u64;
        self.instr_accesses += 1;
        if self.records_accesses() {
            self.record_access(true, addr, value);
        }
        self.write_full(addr, value);
    }
}

#[cfg(test)]
mod tests;
