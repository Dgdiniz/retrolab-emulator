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
//! Cartridge mapping is delegated to a `Box<dyn SnesCartridge>`
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
mod dma;
mod wmdata;

use std::sync::Arc;

use retro_core::cpu::Addressable;

use crate::apu::Apu;
use crate::cartridge::{BusSpeed, MapPlan, MapTarget, SnesCartridge, CX4_CHIP_ID};
use crate::controller::{Controller, StandardPad};
use crate::dma::DmaController;
use crate::ppu::{Ppu, PpuAccessCtx};
use crate::system_regs::{SystemRegWriteFx, SystemRegs};

use b_bus::BBus;
use wmdata::WmdataState;

/// PPU2 hardware version exposed in bits 0-3 of `$213F` STAT78.
/// The original SHVC-CPU shipped with PPU2 v1; later 1-Chip
/// revisions bumped it. We default to 1, the value the reference
/// traces were made with.
const PPU2_VERSION: u8 = 1;

/// Number of pages in the address space (16 MiB / 4 KiB = 4096).
const PAGE_TABLE_LEN: usize = 0x1000;

/// One synchronisation-register access captured by the opt-in I/O
/// trace ([`SnesBus::enable_io_trace`]). Records the CPU access to a
/// watched register (NMI/IRQ status, APU mailbox, INIDISP, the H/V
/// counter latch) together with the program counter doing it and the
/// raster position at the time, so a diagnostic run can pinpoint the
/// spin loop a stalled game is parked in and the condition it awaits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoAccess {
    /// `PBR:PC` (24-bit) of the instruction performing the access.
    pub pc: u32,
    /// Full 24-bit bus address (mirror bank preserved).
    pub addr: u32,
    /// Byte written, or the byte returned to the CPU on a read.
    pub value: u8,
    /// `true` for a store, `false` for a load.
    pub is_write: bool,
    /// Raster scanline (`$213D`-style V counter) at the access.
    pub v: u16,
    /// Raster dot (H counter) at the access.
    pub h: u16,
}

/// Is `addr` one of the synchronisation registers the I/O trace
/// watches? Limited to the MMIO-capable banks (`$00-$3F` / `$80-$BF`)
/// so a ROM/RAM byte that merely shares the low offset isn't logged.
/// The set covers INIDISP (`$2100`), the VRAM address port (`$2116-$2117`),
/// the SLHV latch with the H/V counter readback (`$2137`, `$213C-$213F`),
/// the APU mailbox (`$2140-$2143`), the NMI/IRQ enable and IRQ timer
/// targets (`$4200`, `$4207-$420A`) that an interrupt-driven cutscene VM
/// reprograms, RDNMI/TIMEUP/HVBJOY (`$4210-$4212`), and the general and
/// H-DMA triggers MDMAEN/HDMAEN (`$420B-$420C`) so a VRAM-bound transfer
/// that paces an animation is visible.
#[inline]
fn is_watched_io(addr: u32) -> bool {
    let bank = (addr >> 16) & 0xFF;
    if bank > 0x3F && !(0x80..=0xBF).contains(&bank) {
        return false;
    }
    matches!(
        addr & 0xFFFF,
        0x2100
            | 0x2116..=0x2117 // VMADDL/H (VRAM address port)
            | 0x2137
            | 0x213C..=0x213F
            | 0x2140..=0x2143
            | 0x4200
            | 0x4207..=0x420A
            | 0x4210..=0x4212
            | 0x420B..=0x420C // MDMAEN / HDMAEN (general + H-DMA triggers)
    )
}

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

/// Bounded ring buffer behind [`SnesBus::enable_io_trace`]. Keeps the
/// most recent `cap` accesses so an arbitrarily long run only retains
/// the tail leading up to the inspected frame.
#[derive(Debug, Default)]
struct IoTraceBuf {
    entries: std::collections::VecDeque<IoAccess>,
    cap: usize,
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
    cartridge: Option<Box<dyn SnesCartridge>>,
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
    /// refreshes it before stepping the CPU, but only while a
    /// diagnostic trace is enabled, the fast path never writes it.
    trace_pc: u32,
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
    /// When set, CPU bus reads/writes are appended to [`Self::debug_events`]
    /// so the emulator can match read/write watchpoints and PPU/APU
    /// write events. The debugger arms this (via [`SnesBus::set_debug_armed`])
    /// only when at least one watch/event breakpoint exists, so the fast
    /// path pays nothing.
    debug_armed: bool,
    /// Raw CPU accesses recorded this instruction (drained per step by
    /// `system.rs`). Always empty unless [`Self::debug_armed`] is set.
    debug_events: Vec<BusDebugEvent>,
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

/// A raw bus access recorded for the debugger when watch/event
/// breakpoints are armed.
///
/// The bus records these per CPU access (only while
/// [`SnesBus::debug_armed`] is set) and the emulator drains + matches
/// them after each instruction. `Read`/`Write` carry the full 24-bit
/// address (matched against read/write watchpoints); `PpuRegWrite` and
/// `ApuPortWrite` carry the decoded register/port for the matching SNES
/// event breakpoints. DMA-engine traffic uses the internal typed
/// read/write paths and is intentionally **not** recorded here, only
/// CPU-initiated accesses are observed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BusDebugEvent {
    /// A CPU bus read of `addr` (includes instruction fetches). `value`
    /// is the byte returned, so value-matched read watchpoints can fire.
    Read { addr: u32, value: u8 },
    /// A CPU bus write of `addr`. `value` is the byte stored, so
    /// value-matched write watchpoints can fire.
    Write { addr: u32, value: u8 },
    /// A write to PPU register `$2100 | reg` (`reg` is `$00-$3F`).
    PpuRegWrite { reg: u8 },
    /// A write to APU mailbox port `port` (0-3), i.e. `$2140-$2143`.
    ApuPortWrite { port: u8 },
}

/// Classify a CPU write address into the PPU-register / APU-port event
/// it represents, if any. Only the system-area B-bus window (`$2100-
/// $21FF` in banks `$00-$3F` / `$80-$BF`) qualifies, the same range
/// the page table routes to [`PageHandler::BBus`]. WRAM banks `$7E/$7F`
/// (where `$2100` is plain RAM) are excluded.
#[inline]
fn classify_b_bus_write(addr: u32) -> Option<BusDebugEvent> {
    let bank = (addr >> 16) & 0xFF;
    let in_system_area = bank <= 0x3F || (0x80..=0xBF).contains(&bank);
    if !in_system_area {
        return None;
    }
    match addr & 0xFFFF {
        // PPU registers $2100-$213F.
        off @ 0x2100..=0x213F => Some(BusDebugEvent::PpuRegWrite {
            reg: (off & 0xFF) as u8,
        }),
        // APU mailbox $2140-$2143, mirrored across $2140-$217F.
        off @ 0x2140..=0x217F => Some(BusDebugEvent::ApuPortWrite {
            port: (off & 0x03) as u8,
        }),
        _ => None,
    }
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
            debug_events: Vec::new(),
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

    /// Arm or disarm bus-level debug recording. When `armed`, CPU
    /// reads/writes are captured into [`Self::debug_events`]; disarming
    /// clears any pending records so a later session starts clean.
    /// Mirrored from the debugger's watch/event-breakpoint state once
    /// per frame by `system.rs`.
    pub fn set_debug_armed(&mut self, armed: bool) {
        self.debug_armed = armed;
        if !armed {
            self.debug_events.clear();
        }
    }

    /// Whether bus-level debug recording is currently armed.
    pub fn debug_armed(&self) -> bool {
        self.debug_armed
    }

    /// Borrow the raw bus accesses recorded since the last
    /// [`SnesBus::clear_debug_events`].
    pub fn debug_events(&self) -> &[BusDebugEvent] {
        &self.debug_events
    }

    /// Drop all recorded bus accesses. The emulator clears the buffer
    /// before each measured instruction (so debug-capture reads don't
    /// leak in) and after draining it.
    pub fn clear_debug_events(&mut self) {
        self.debug_events.clear();
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
        // the hardware sequence even though `read_auto` reads the raw
        // snapshot directly.
        for pad in &mut self.controllers {
            pad.strobe(true);
            pad.strobe(false);
        }
        let joy1 = self.controllers[0].read_auto();
        let joy2 = self.controllers[1].read_auto();
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

    /// Start (or stop) capturing the host<->coprocessor register stream.
    /// Enabling clears any previously captured log. See
    /// [`Self::coprocessor_trace`].
    pub fn enable_coprocessor_trace(&mut self, on: bool) {
        self.coprocessor_trace = if on { Some(Vec::new()) } else { None };
    }

    /// Drain and return the captured `(is_write, offset, byte)` stream,
    /// leaving capture disabled. Empty when capture was never enabled.
    pub fn take_coprocessor_trace(&mut self) -> Vec<(bool, u32, u8)> {
        self.coprocessor_trace.take().unwrap_or_default()
    }

    /// Start capturing the synchronisation-register access ring buffer,
    /// retaining the most recent `cap` entries (see [`IoAccess`]). A
    /// `cap` of 0 is clamped to 1. Enabling clears any prior capture.
    pub fn enable_io_trace(&mut self, cap: usize) {
        self.io_trace = Some(IoTraceBuf {
            entries: std::collections::VecDeque::new(),
            cap: cap.max(1),
        });
    }

    /// Stop capturing the I/O trace and discard whatever was buffered.
    pub fn disable_io_trace(&mut self) {
        self.io_trace = None;
    }

    /// Start capturing the instruction-PC ring buffer, retaining the
    /// most recent `cap` program counters (see [`Self::pc_trace`]). A
    /// `cap` of 0 is clamped to 1. Enabling clears any prior capture.
    pub fn enable_pc_trace(&mut self, cap: usize) {
        self.pc_trace = Some((std::collections::VecDeque::new(), cap.max(1)));
    }

    /// Drain and return the captured PC ring buffer (oldest first),
    /// leaving capture disabled. Empty when capture was never enabled.
    pub fn take_pc_trace(&mut self) -> Vec<u32> {
        self.pc_trace
            .take()
            .map(|(ring, _)| ring.into_iter().collect())
            .unwrap_or_default()
    }

    /// `true` while any diagnostic trace (I/O, PC, or address watch) is
    /// active. `system.rs` checks this before stamping
    /// [`Self::set_trace_pc`] so the fast path pays nothing when all
    /// are off.
    #[inline]
    pub fn diag_trace_enabled(&self) -> bool {
        self.io_trace.is_some() || self.pc_trace.is_some() || self.addr_watch.is_some()
    }

    /// Start watching the WRAM cell that `addr` resolves to (via
    /// [`wram_offset`], so any mirror alias is caught), retaining the
    /// most recent `cap` reads/writes. A `cap` of 0 is clamped to 1.
    /// Enabling clears any prior watch. Non-WRAM `addr` watches nothing.
    pub fn enable_addr_watch(&mut self, addr: u32, cap: usize) {
        if let Some(off) = wram_offset(addr) {
            self.addr_watch = Some((
                off,
                IoTraceBuf {
                    entries: std::collections::VecDeque::new(),
                    cap: cap.max(1),
                },
            ));
        }
    }

    /// Drain and return the captured address-watch ring buffer (oldest
    /// first), leaving the watch disabled. Empty when never enabled.
    pub fn take_addr_watch(&mut self) -> Vec<IoAccess> {
        self.addr_watch
            .take()
            .map(|(_, buf)| buf.entries.into_iter().collect())
            .unwrap_or_default()
    }

    /// Record the `PBR:PC` of the instruction about to execute: stamped
    /// onto any [`IoAccess`] it produces, and pushed into the PC ring
    /// buffer when that capture is active.
    #[inline]
    pub fn set_trace_pc(&mut self, pc: u32) {
        self.trace_pc = pc;
        if let Some((ring, cap)) = self.pc_trace.as_mut() {
            if ring.len() == *cap {
                ring.pop_front();
            }
            ring.push_back(pc);
        }
    }

    /// Drain and return the captured access ring buffer (oldest first),
    /// leaving capture disabled. Empty when capture was never enabled.
    pub fn take_io_trace(&mut self) -> Vec<IoAccess> {
        self.io_trace
            .take()
            .map(|buf| buf.entries.into_iter().collect())
            .unwrap_or_default()
    }

    /// Append one access to the ring buffer, evicting the oldest entry
    /// once `cap` is reached. Only called from the CPU read/write path
    /// when capture is enabled and the address is watched.
    #[inline]
    fn record_io_access(&mut self, is_write: bool, addr: u32, value: u8) {
        let (pc, v, h) = (self.trace_pc, self.cached_v_scanline, self.cached_h_dot);
        if let Some(buf) = self.io_trace.as_mut() {
            if buf.entries.len() == buf.cap {
                buf.entries.pop_front();
            }
            buf.entries.push_back(IoAccess {
                pc,
                addr,
                value,
                is_write,
                v,
                h,
            });
        }
    }

    /// Append one access to the address-watch ring buffer when `addr`
    /// resolves to the watched WRAM offset. Only called from the CPU
    /// read/write path when the watch is enabled.
    #[inline]
    fn record_addr_watch(&mut self, is_write: bool, addr: u32, value: u8) {
        let (pc, v, h) = (self.trace_pc, self.cached_v_scanline, self.cached_h_dot);
        if let Some((off, buf)) = self.addr_watch.as_mut() {
            if wram_offset(addr) != Some(*off) {
                return;
            }
            if buf.entries.len() == buf.cap {
                buf.entries.pop_front();
            }
            buf.entries.push_back(IoAccess {
                pc,
                addr,
                value,
                is_write,
                v,
                h,
            });
        }
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
    pub fn install_cartridge(&mut self, cartridge: Box<dyn SnesCartridge>) {
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
    pub fn cartridge_ref(&self) -> Option<&dyn SnesCartridge> {
        self.cartridge.as_deref()
    }

    /// Mutable borrow of the installed cartridge, if any.
    pub fn cartridge_mut(&mut self) -> Option<&mut (dyn SnesCartridge + 'static)> {
        self.cartridge.as_deref_mut()
    }

    /// The installed cartridge's [`MapPlan`], or `None` when no
    /// cartridge is installed. Used by the cartridge debugger to render
    /// the memory map and ROM/SRAM regions.
    pub fn cartridge_plan(&self) -> Option<&MapPlan> {
        self.cartridge_plan.as_deref()
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
    pub fn coprocessors_savestate(&self) -> Vec<(u8, crate::savestate::CoprocessorState)> {
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
        states: &[(u8, crate::savestate::CoprocessorState)],
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
            controllers: [
                self.controllers[0].to_savestate(),
                self.controllers[1].to_savestate(),
            ],
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
        self.controllers[0].from_savestate(&s.controllers[0]);
        self.controllers[1].from_savestate(&s.controllers[1]);
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
        let value = {
            let cart = match self.cartridge.as_mut() {
                Some(c) => c,
                None => return mdr,
            };
            match target {
                MapTarget::Rom { base_offset } => cart.read_rom(base_offset + target_offset),
                MapTarget::Sram { base_offset } => cart.read_sram(base_offset + target_offset),
                MapTarget::ChipMmio { chip_id, base } => {
                    cart.read_chip(chip_id, base + target_offset, chip_cycle)
                }
                MapTarget::OpenBus => mdr,
            }
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
        {
            let cart = match self.cartridge.as_mut() {
                Some(c) => c,
                None => return,
            };
            match target {
                // Cartridge ROM is read-only; SNES ROM "writes" are how
                // some chips (SA-1, SuperFX) signal their MMIO via the
                // ChipMmio target rather than a Rom write.
                MapTarget::Rom { .. } => {}
                MapTarget::Sram { base_offset } => {
                    cart.write_sram(base_offset + target_offset, value)
                }
                MapTarget::ChipMmio { chip_id, base } => {
                    cart.write_chip(chip_id, base + target_offset, value, chip_cycle)
                }
                MapTarget::OpenBus => {}
            }
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
    /// so an open-bus write still leaves the value latched.
    #[inline]
    fn write_inner(&mut self, addr: u32, value: u8) {
        let addr24 = addr & 0x00FF_FFFF;
        self.mdr = value;
        match self.page_table[(addr24 >> 12) as usize] {
            PageHandler::Wram => self.wram[(addr24 - 0x7E_0000) as usize] = value,
            PageHandler::WramMirror => self.wram[(addr24 & 0x1FFF) as usize] = value,
            PageHandler::BBus => self.write_b_bus_window(addr24, value),
            PageHandler::CpuMmio => self.write_cpu_mmio(addr24, value),
            PageHandler::Cartridge { slot_index } => {
                self.write_cartridge(slot_index, addr24, value)
            }
            PageHandler::OpenBus => { /* no-op: MDR already latched above */ }
        }
    }

    /// Fastest read tier (`ExecutionTier::Fast`): pure dispatch,
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

    /// Fastest write tier. Mirror of [`SnesBus::read_fast`].
    #[inline(always)]
    pub fn write_fast(&mut self, addr: u32, value: u8) {
        self.write_inner(addr, value);
    }

    /// Light-instrumentation write tier. Mirror of
    /// [`SnesBus::read_light`].
    pub fn write_light(&mut self, addr: u32, value: u8) {
        self.write_inner(addr, value);
    }

    /// Full-instrumentation write tier. Mirror of
    /// [`SnesBus::read_full`].
    pub fn write_full(&mut self, addr: u32, value: u8) {
        self.write_inner(addr, value);
    }
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
        if self.debug_armed {
            self.debug_events.push(BusDebugEvent::Read { addr, value });
        }
        if self.io_trace.is_some() && is_watched_io(addr) {
            self.record_io_access(false, addr, value);
        }
        if self.addr_watch.is_some() {
            self.record_addr_watch(false, addr, value);
        }
        value
    }

    /// Delegates to [`SnesBus::write_full`] (the safe default).
    fn write(&mut self, addr: u32, value: u8) {
        self.instr_master_cycles += self.cycles_for(addr) as u64;
        self.instr_accesses += 1;
        if self.debug_armed {
            self.debug_events.push(BusDebugEvent::Write { addr, value });
            if let Some(ev) = classify_b_bus_write(addr) {
                self.debug_events.push(ev);
            }
        }
        if self.io_trace.is_some() && is_watched_io(addr) {
            self.record_io_access(true, addr, value);
        }
        if self.addr_watch.is_some() {
            self.record_addr_watch(true, addr, value);
        }
        self.write_full(addr, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[inline]
    fn make_addr(bank: u8, offset: u16) -> u32 {
        ((bank as u32) << 16) | (offset as u32)
    }

    #[test]
    fn wram_mirror_low_bank_low_half() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_0000, 0x42);
        assert_eq!(bus.read(0x00_0000), 0x42);
    }

    #[test]
    fn io_trace_records_only_watched_accesses_with_pc() {
        let mut bus = SnesBus::new();
        bus.enable_io_trace(16);
        assert!(bus.diag_trace_enabled());
        bus.set_trace_pc(0x12_3456);

        // Watched: INIDISP write + RDNMI read (both MMIO-bank addresses).
        bus.write(0x00_2100, 0x0F);
        let _ = bus.read(0x00_4210);
        // Not watched: a WRAM byte and a VRAM-data port write.
        bus.write(0x7E_0000, 0xAB);
        bus.write(0x00_2118, 0xCD);
        // Same low offset but a non-MMIO bank ($40) must be ignored.
        bus.write(0x40_2100, 0x77);

        let trace = bus.take_io_trace();
        assert_eq!(trace.len(), 2, "only the two watched accesses logged");
        assert_eq!(trace[0].pc, 0x12_3456);
        assert!(trace[0].is_write);
        assert_eq!(trace[0].addr, 0x00_2100);
        assert_eq!(trace[0].value, 0x0F);
        assert!(!trace[1].is_write);
        assert_eq!(trace[1].addr, 0x00_4210);
        // Capture is one-shot: taking it disables further recording.
        assert!(!bus.diag_trace_enabled());
    }

    #[test]
    fn io_trace_ring_buffer_keeps_only_the_tail() {
        let mut bus = SnesBus::new();
        bus.enable_io_trace(3);
        for i in 0..10u8 {
            bus.set_trace_pc(0xC0_0000 + i as u32);
            bus.write(0x00_2100, i);
        }
        let trace = bus.take_io_trace();
        assert_eq!(trace.len(), 3, "ring capped at cap");
        // Oldest-first: the last three writes (values 7,8,9) survive.
        assert_eq!(
            trace.iter().map(|a| a.value).collect::<Vec<_>>(),
            vec![7, 8, 9]
        );
    }

    #[test]
    fn wram_offset_resolves_direct_banks_and_low_mirror() {
        // Direct $7E/$7F banks.
        assert_eq!(wram_offset(0x7E_0158), Some(0x0158));
        assert_eq!(wram_offset(0x7F_0000), Some(0x1_0000));
        // Low-8 KiB mirror in system banks (both halves of the map).
        assert_eq!(wram_offset(0x00_0158), Some(0x0158));
        assert_eq!(wram_offset(0xB0_1FFF), Some(0x1FFF));
        // Above $1FFF in a system bank is cartridge/open-bus, not WRAM.
        assert_eq!(wram_offset(0x00_2000), None);
        assert_eq!(wram_offset(0x40_0158), None);
    }

    #[test]
    fn addr_watch_catches_a_wram_cell_through_every_alias() {
        let mut bus = SnesBus::new();
        bus.enable_addr_watch(0x7E_0158, 16);
        assert!(bus.diag_trace_enabled());
        bus.set_trace_pc(0xC0_EC55);

        // Write via the direct bank, read back via the low mirror.
        bus.write(0x7E_0158, 0x80);
        let _ = bus.read(0x00_0158);
        // Unrelated WRAM cells must not be logged.
        bus.write(0x7E_0159, 0x11);
        bus.write(0x00_0000, 0x22);

        let trace = bus.take_addr_watch();
        assert_eq!(trace.len(), 2, "only the two accesses to $0158 logged");
        assert!(trace[0].is_write);
        assert_eq!(trace[0].value, 0x80);
        assert_eq!(trace[0].pc, 0xC0_EC55);
        assert!(!trace[1].is_write);
        assert!(!bus.diag_trace_enabled());
    }

    #[test]
    fn pc_trace_captures_recent_program_counters() {
        let mut bus = SnesBus::new();
        bus.enable_pc_trace(4);
        assert!(bus.diag_trace_enabled());
        for pc in [0x10u32, 0x11, 0x12, 0x13, 0x14, 0x15] {
            bus.set_trace_pc(pc);
        }
        let pcs = bus.take_pc_trace();
        assert_eq!(pcs, vec![0x12, 0x13, 0x14, 0x15], "tail of the PC stream");
        assert!(!bus.diag_trace_enabled());
    }

    #[test]
    fn wram_mirror_low_bank_high_half() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_1FFF, 0x99);
        assert_eq!(bus.read(0x00_1FFF), 0x99);
    }

    #[test]
    fn wram_mirror_high_bank() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_0500, 0x37);
        assert_eq!(bus.read(0x80_0500), 0x37);
        assert_eq!(bus.read(0xB0_0500), 0x37);
    }

    #[test]
    fn wram_no_mirror_beyond_2000() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_5000, 0x77);
        // Direct read of physical WRAM sees the byte we wrote.
        assert_eq!(bus.read(0x7E_5000), 0x77);
        // After that read, MDR = 0x77. The system-area read at
        // $00:5000 lives in the OpenBus page, so it should return
        // the current MDR (0x77) rather than the WRAM byte.
        assert_eq!(bus.read(0x00_5000), 0x77);
        // To prove it is open-bus and not a mirror, flip MDR via a
        // write to a different open-bus address and read again.
        bus.write(0x00_6000, 0xAA);
        assert_eq!(bus.read(0x00_5000), 0xAA);
    }

    #[test]
    fn wram_direct_full_range_endpoints() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_0000, 0x11);
        bus.write(0x7F_FFFF, 0x22);
        assert_eq!(bus.read(0x7E_0000), 0x11);
        assert_eq!(bus.read(0x7F_FFFF), 0x22);
        // Middle of the WRAM image.
        bus.write(0x7E_FFFF, 0x33);
        bus.write(0x7F_0000, 0x44);
        assert_eq!(bus.read(0x7E_FFFF), 0x33);
        assert_eq!(bus.read(0x7F_0000), 0x44);
    }

    #[test]
    fn mdr_starts_at_zero() {
        let mut bus = SnesBus::new();
        // $00:FF00 is in the cartridge area, which defaults to
        // OpenBus before any cartridge install, so the result is
        // whatever MDR currently holds, 0 on a fresh bus.
        assert_eq!(bus.read(0x00_FF00), 0x00);
        assert_eq!(bus.mdr(), 0x00);
    }

    #[test]
    fn mdr_updates_on_read() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_0000, 0x55);
        // Reading WRAM sets MDR to the WRAM byte.
        assert_eq!(bus.read(0x7E_0000), 0x55);
        // Now reading from an open-bus address surfaces the latched
        // MDR (no other side effects).
        assert_eq!(bus.read(0x00_FF00), 0x55);
        assert_eq!(bus.mdr(), 0x55);
    }

    #[test]
    fn mdr_updates_on_write() {
        let mut bus = SnesBus::new();
        // $00:FF00 is OpenBus; the write only latches MDR.
        bus.write(0x00_FF00, 0xAA);
        assert_eq!(bus.mdr(), 0xAA);
        // Reading another OpenBus address surfaces the latched MDR.
        assert_eq!(bus.read(0x00_FE00), 0xAA);
    }

    #[test]
    fn b_bus_window_low_write_only_returns_mdr() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_0000, 0xC3);
        let _ = bus.read(0x7E_0000); // latch MDR to 0xC3

        // Write-only PPU registers that fall through to the
        // catch-all "return A-bus MDR" branch. The PPU1 MDR-only
        // subset and the
        // read-only registers ($2134-$2136 MPY, $2138-$213B OAM/
        // VRAM/CGRAM reads, $213E STAT77) exercise different paths
        // and live in their own tests.
        for offset in [
            0x2100u16, 0x2101, 0x2102, 0x2103, 0x2107, 0x210B, 0x210C, 0x210D, 0x210E, 0x210F,
            0x2110, 0x2111, 0x2112, 0x2113, 0x2117, 0x211B, 0x211C, 0x211D, 0x211E, 0x211F, 0x2120,
            0x2121, 0x2122, 0x2123, 0x2127, 0x212B, 0x212C, 0x212D, 0x212E, 0x212F, 0x2130, 0x2131,
            0x2132, 0x2133,
        ] {
            assert_eq!(
                bus.read(make_addr(0x00, offset)),
                0xC3,
                "write-only ${offset:04X} should return A-bus MDR"
            );
        }
        // Expansion / reserved range ($2184-$21FF): open bus = MDR.
        for offset in 0x2184u16..=0x21FF {
            assert_eq!(bus.read(make_addr(0x00, offset)), 0xC3);
        }
    }

    #[test]
    fn b_bus_window_high_write_only_returns_mdr() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_0000, 0x5A);
        let _ = bus.read(0x7E_0000);
        // Mirror in the $80-$BF half. Same surface.
        for offset in [0x2100u16, 0x2101, 0x2107, 0x2117, 0x2123, 0x2130, 0x2133] {
            assert_eq!(bus.read(make_addr(0x80, offset)), 0x5A);
        }
        for offset in 0x2184u16..=0x21FF {
            assert_eq!(bus.read(make_addr(0x80, offset)), 0x5A);
        }
    }

    #[test]
    fn b_bus_window_ppu1_mdr_only_registers_return_ppu1_mdr() {
        let mut bus = SnesBus::new();
        // Latch A-bus MDR to a recognisable value.
        bus.write(0x7E_0000, 0xC3);
        let _ = bus.read(0x7E_0000);
        // The 18 write-only addresses whose read returns the PPU1
        // latch (checked against bsnes-plus). PPU1 MDR starts at 0 so
        // the reads should surface 0, NOT the A-bus MDR.
        for offset in [
            0x2104u16, 0x2105, 0x2106, 0x2108, 0x2109, 0x210A, 0x2114, 0x2115, 0x2116, 0x2118,
            0x2119, 0x211A, 0x2124, 0x2125, 0x2126, 0x2128, 0x2129, 0x212A,
        ] {
            assert_eq!(
                bus.read(make_addr(0x00, offset)),
                0,
                "PPU1 MDR-only ${offset:04X} should return PPU1 MDR (0 at boot), not A-bus MDR"
            );
        }
    }

    #[test]
    fn cpu_mmio_write_only_system_regs_return_mdr() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_0000, 0x99);
        let _ = bus.read(0x7E_0000);
        // $4200-$420F are write-only system registers -> reads surface
        // the MDR latch.
        assert_eq!(bus.read(0x00_4200), 0x99);
        // A write to CPU MMIO should latch MDR without panicking.
        bus.write(0x00_4200, 0x66);
        assert_eq!(bus.mdr(), 0x66);
    }

    #[test]
    fn cartridge_slot_returns_mdr_when_no_cart() {
        let mut bus = SnesBus::new();
        bus.write(0x7E_0000, 0x77);
        let _ = bus.read(0x7E_0000);
        // Without a cartridge installed every page outside the
        // system area is OpenBus, so $80:8000 surfaces MDR.
        assert_eq!(bus.read(0x80_8000), 0x77);
    }

    #[test]
    fn page_handler_inspection_matches_default_table() {
        let bus = SnesBus::new();
        assert_eq!(bus.page_handler(0x7E_0000), PageHandler::Wram);
        assert_eq!(bus.page_handler(0x7F_FFFF), PageHandler::Wram);
        assert_eq!(bus.page_handler(0x00_0000), PageHandler::WramMirror);
        assert_eq!(bus.page_handler(0x00_1FFF), PageHandler::WramMirror);
        assert_eq!(bus.page_handler(0xBF_0000), PageHandler::WramMirror);
        assert_eq!(bus.page_handler(0x00_2100), PageHandler::BBus);
        assert_eq!(bus.page_handler(0x80_2100), PageHandler::BBus);
        assert_eq!(bus.page_handler(0x00_4200), PageHandler::CpuMmio);
        assert_eq!(bus.page_handler(0x80_4200), PageHandler::CpuMmio);
        assert_eq!(bus.page_handler(0x80_8000), PageHandler::OpenBus);
        assert_eq!(bus.page_handler(0xC0_0000), PageHandler::OpenBus);
    }

    // -- Walker / cartridge / speed-table tests -------------

    use crate::cartridge::{BusSpeed, MapPlan, MapSlot, MapTarget, PlainCartridge};

    fn lorom_like_plan(speed: BusSpeed) -> MapPlan {
        let mut plan = MapPlan::new("lorom-like");
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            speed,
        ));
        plan
    }

    fn ramp_rom(size: usize) -> Vec<u8> {
        (0..size).map(|i| (i & 0xFF) as u8).collect()
    }

    #[test]
    fn walker_promotes_open_bus_to_cartridge() {
        let plan = lorom_like_plan(BusSpeed::Fast);
        let cart = PlainCartridge::new(ramp_rom(0x8000), 0, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));

        assert_eq!(
            bus.page_handler(0x80_8000),
            PageHandler::Cartridge { slot_index: 0 }
        );
        assert_eq!(
            bus.page_handler(0xFF_F000),
            PageHandler::Cartridge { slot_index: 0 }
        );
        // Cart was promoted only inside $8000-$FFFF: $80:7FFF is in
        // bank $80 page $7, which is OpenBus in the default table
        // and is NOT covered by this slot.
        assert_eq!(bus.page_handler(0x80_7000), PageHandler::OpenBus);
    }

    #[test]
    fn walker_preserves_system_pages() {
        // A slot covering the *entire* system area must not stomp
        // on Wram / WramMirror / BBus / CpuMmio. The walker's
        // first-wins-on-OpenBus rule is what guarantees this.
        let mut plan = MapPlan::new("aggressive");
        plan.push(MapSlot::new(
            0x00..=0xFF,
            0x0000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        let cart = PlainCartridge::new(ramp_rom(0x100), 0, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));

        // Wram banks $7E-$7F untouched.
        assert_eq!(bus.page_handler(0x7E_0000), PageHandler::Wram);
        assert_eq!(bus.page_handler(0x7F_FFFF), PageHandler::Wram);
        // WramMirror in low system-area banks untouched.
        assert_eq!(bus.page_handler(0x00_0000), PageHandler::WramMirror);
        assert_eq!(bus.page_handler(0xBF_1FFF), PageHandler::WramMirror);
        // B-bus window page untouched.
        assert_eq!(bus.page_handler(0x00_2100), PageHandler::BBus);
        assert_eq!(bus.page_handler(0x80_2100), PageHandler::BBus);
        // CPU MMIO page untouched.
        assert_eq!(bus.page_handler(0x00_4200), PageHandler::CpuMmio);
        // But pages that *were* OpenBus inside the system area do
        // get promoted (e.g. $3000-$3FFF, $5000-$7FFF, $8000-$FFFF).
        assert_eq!(
            bus.page_handler(0x00_8000),
            PageHandler::Cartridge { slot_index: 0 }
        );
        assert_eq!(
            bus.page_handler(0x00_3000),
            PageHandler::Cartridge { slot_index: 0 }
        );
    }

    #[test]
    fn walker_first_wins_between_slots() {
        let mut plan = MapPlan::new("overlap");
        // First slot wins.
        plan.push(MapSlot::new(
            0x80..=0x80,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        // Second slot overlaps but should be silently ignored on
        // the pages already covered by slot 0.
        plan.push(MapSlot::new(
            0x80..=0x80,
            0x8000..=0xFFFF,
            MapTarget::Rom {
                base_offset: 0x8000,
            },
            BusSpeed::Slow,
        ));
        let cart = PlainCartridge::new(ramp_rom(0x10000), 0, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));

        assert_eq!(
            bus.page_handler(0x80_8000),
            PageHandler::Cartridge { slot_index: 0 }
        );
        // At install time the bus pins FastROM-eligible pages to
        // Slow (8), MEMSEL boots disabled. Flipping MEMSEL on
        // demonstrates that the FastROM-eligible cache picked up
        // slot 0 (Fast -> 6), not slot 1 (Slow -> would never go to
        // 6 because Slow slots are excluded from the cache).
        assert_eq!(bus.cycles_for(0x80_8000), 8);
        bus.apply_memsel(true);
        assert_eq!(bus.cycles_for(0x80_8000), 6);
    }

    #[test]
    fn speed_table_default() {
        let bus = SnesBus::new();
        // Low RAM / WRAM mirror in the I/O banks -> Slow (8).
        assert_eq!(bus.cycles_for(0x00_0000), 8);
        assert_eq!(bus.cycles_for(0xBF_1FFF), 8);
        // B-bus window ($2000-$3FFF: PPU/APU/WMDATA) -> Fast (6).
        assert_eq!(bus.cycles_for(0x00_2100), 6);
        assert_eq!(bus.cycles_for(0x00_2140), 6);
        // Manual joypad window ($4000-$41FF) -> XSlow (12).
        assert_eq!(bus.cycles_for(0x00_4000), 12);
        assert_eq!(bus.cycles_for(0x80_4016), 12);
        // CPU MMIO / DMA register file ($4200-$5FFF) -> Fast (6),
        // including $4212 HVBJOY and the $4218-$421F auto-joypad
        // results that boot loops poll heavily.
        assert_eq!(bus.cycles_for(0x80_4200), 6);
        assert_eq!(bus.cycles_for(0x00_4212), 6);
        assert_eq!(bus.cycles_for(0x00_4300), 6);
        // $6000-$7FFF expansion -> Slow (8).
        assert_eq!(bus.cycles_for(0x00_6000), 8);
        // ROM region defaults to Slow (8) until a cart/MEMSEL retunes it.
        assert_eq!(bus.cycles_for(0x80_8000), 8);
        // Direct WRAM banks $7E-$7F are Slow as well.
        assert_eq!(bus.cycles_for(0x7E_0000), 8);
    }

    #[test]
    fn speed_table_after_install_uses_slot_speed() {
        let cart = PlainCartridge::new(ramp_rom(0x8000), 0, lorom_like_plan(BusSpeed::XSlow));
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));
        // Every promoted page takes the slot's declared speed, XSlow
        // (12) here; MEMSEL only rewrites pages of `Fast` slots.
        assert_eq!(bus.cycles_for(0x80_8000), 12);
        assert_eq!(bus.cycles_for(0xFF_F000), 12);
        // Pages still OpenBus (e.g. $80:7000) keep the default Slow
        // because the walker only overwrote promoted pages.
        assert_eq!(bus.cycles_for(0x80_7000), 8);
    }

    #[test]
    fn cartridge_rom_round_trip() {
        // 32 KiB ROM with a ramp pattern, slot $80:$8000-$FFFF.
        // The slot's `compute_target_offset` returns
        // (addr - $8000) & 0xFFFF, which is what we want when the
        // ROM is exactly 32 KiB and the slot is a single bank.
        let cart = PlainCartridge::new(ramp_rom(0x8000), 0, lorom_like_plan(BusSpeed::Fast));
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));
        // ROM[0] = 0x00 at $80:8000.
        assert_eq!(bus.read(0x80_8000), 0x00);
        // ROM[$7FFF] = 0xFF at $80:FFFF.
        assert_eq!(bus.read(0x80_FFFF), 0xFF);
        // Read updates MDR.
        assert_eq!(bus.mdr(), 0xFF);
    }

    #[test]
    fn cartridge_sram_round_trip() {
        // SRAM at LoROM-style $70:$0000-$7FFF (32 KiB).
        let mut plan = MapPlan::new("sram-lorom");
        plan.push(MapSlot::new(
            0x70..=0x70,
            0x0000..=0x7FFF,
            MapTarget::Sram { base_offset: 0 },
            BusSpeed::Slow,
        ));
        let cart = PlainCartridge::new(ramp_rom(0), 0x8000, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));
        bus.write(0x70_0000, 0xAB);
        bus.write(0x70_7FFF, 0xCD);
        assert_eq!(bus.read(0x70_0000), 0xAB);
        assert_eq!(bus.read(0x70_7FFF), 0xCD);
    }

    #[test]
    fn cartridge_open_bus_slot_returns_mdr() {
        let mut plan = MapPlan::new("open-bus-slot");
        plan.push(MapSlot::new(
            0x80..=0x80,
            0x8000..=0xFFFF,
            MapTarget::OpenBus,
            BusSpeed::Fast,
        ));
        let cart = PlainCartridge::new(ramp_rom(0x8000), 0, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));
        // The page IS promoted (handler became Cartridge), but the
        // slot's target is OpenBus, so reads return MDR.
        assert_eq!(
            bus.page_handler(0x80_8000),
            PageHandler::Cartridge { slot_index: 0 }
        );
        bus.write(0x7E_0000, 0x5A);
        let _ = bus.read(0x7E_0000); // MDR = 0x5A
        assert_eq!(bus.read(0x80_8000), 0x5A);
    }

    #[test]
    fn wmdata_round_trip_through_bus() {
        let mut bus = SnesBus::new();
        // WMADD = $00:0000 (i.e. $7E:0000).
        bus.write(0x00_2181, 0x00);
        bus.write(0x00_2182, 0x00);
        bus.write(0x00_2183, 0x00);
        // Write through WMDATA.
        bus.write(0x00_2180, 0x42);
        // Underlying WRAM byte at $7E:0000 was updated.
        assert_eq!(bus.read(0x7E_0000), 0x42);
    }

    #[test]
    fn wmdata_auto_increment_through_bus() {
        let mut bus = SnesBus::new();
        // Reset WMADD to 0.
        bus.write(0x00_2181, 0x00);
        bus.write(0x00_2182, 0x00);
        bus.write(0x00_2183, 0x00);
        bus.write(0x00_2180, 0x11); // -> $7E:0000 = 0x11, WMADD = 1
        bus.write(0x00_2180, 0x22); // -> $7E:0001 = 0x22, WMADD = 2
        assert_eq!(bus.read(0x7E_0000), 0x11);
        assert_eq!(bus.read(0x7E_0001), 0x22);

        // Reads also auto-increment. Reset WMADD to 0, then read
        // twice through WMDATA and confirm we got both bytes.
        bus.write(0x00_2181, 0x00);
        bus.write(0x00_2182, 0x00);
        bus.write(0x00_2183, 0x00);
        assert_eq!(bus.read(0x00_2180), 0x11);
        assert_eq!(bus.read(0x00_2180), 0x22);
    }

    // -- System registers + MEMSEL + HV + tier dispatch ---------

    fn fastrom_lorom_like_plan() -> MapPlan {
        let mut plan = MapPlan::new("fastrom-lorom");
        plan.push(MapSlot::new(
            0x80..=0xBF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        plan
    }

    fn install_fastrom_lorom(bus: &mut SnesBus) {
        let cart = PlainCartridge::new(ramp_rom(0x40 * 0x8000), 0, fastrom_lorom_like_plan());
        bus.install_cartridge(Box::new(cart));
    }

    #[test]
    fn system_regs_dispatch_through_bus() {
        let mut bus = SnesBus::new();
        // Write WRMPYA + WRMPYB through the bus to trigger an
        // unsigned 8x8 multiply, then read RDMPY through the bus.
        bus.write(0x00_4202, 12);
        bus.write(0x00_4203, 11); // 12 * 11 = 132 = 0x0084
        assert_eq!(bus.read(0x00_4216), 0x84);
        assert_eq!(bus.read(0x00_4217), 0x00);
        // The accessor agrees with the bus dispatch.
        assert_eq!(bus.system_regs().math_cycles_remaining(), 8);
    }

    #[test]
    fn memsel_rebuilds_fastrom_speed_on_toggle() {
        let mut bus = SnesBus::new();
        install_fastrom_lorom(&mut bus);
        // Boot: MEMSEL=0 -> FastROM region pinned to Slow (8).
        assert_eq!(bus.cycles_for(0x80_8000), 8);
        assert_eq!(bus.cycles_for(0xBF_FFFF), 8);
        // Enable FastROM via $420D bit 0 through the bus.
        bus.write(0x00_420D, 0x01);
        assert!(bus.memsel_fast());
        assert_eq!(bus.cycles_for(0x80_8000), 6);
        assert_eq!(bus.cycles_for(0xBF_FFFF), 6);
        // Toggle back to Slow.
        bus.write(0x00_420D, 0x00);
        assert!(!bus.memsel_fast());
        assert_eq!(bus.cycles_for(0x80_8000), 8);
    }

    #[test]
    fn memsel_switches_banks_c0_to_ff_too() {
        // Banks $C0-$FF belong to the FastROM region whole, both
        // halves of each bank: HiROM code running there is as fast as
        // code running from $80-$BF.
        let mut plan = MapPlan::new("fastrom-hirom");
        plan.push(MapSlot::new(
            0xC0..=0xFF,
            0x0000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        let cart = PlainCartridge::new(ramp_rom(0x40_0000), 0, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));
        // Boot: MEMSEL=0 -> pinned to Slow (8).
        assert_eq!(bus.cycles_for(0xC0_0000), 8);
        assert_eq!(bus.cycles_for(0xFF_FFFF), 8);
        bus.write(0x00_420D, 0x01);
        assert_eq!(bus.cycles_for(0xC0_0000), 6);
        assert_eq!(bus.cycles_for(0xC0_7FFF), 6);
        assert_eq!(bus.cycles_for(0xFF_FFFF), 6);
        bus.write(0x00_420D, 0x00);
        assert_eq!(bus.cycles_for(0xC0_0000), 8);
        assert_eq!(bus.cycles_for(0xFF_FFFF), 8);
    }

    #[test]
    fn memsel_no_effect_on_non_fastrom_slot() {
        let mut plan = MapPlan::new("slow-fastrom-region");
        plan.push(MapSlot::new(
            0x80..=0xBF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            // Cartridge declares Slow -> not FastROM-capable
            // hardware, so MEMSEL must be a no-op here.
            BusSpeed::Slow,
        ));
        let cart = PlainCartridge::new(ramp_rom(0x40 * 0x8000), 0, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));
        assert_eq!(bus.cycles_for(0x80_8000), 8);
        bus.write(0x00_420D, 0x01);
        assert_eq!(bus.cycles_for(0x80_8000), 8);
    }

    #[test]
    fn memsel_no_effect_on_non_fastrom_region() {
        // Slot is declared Fast but mapped into banks $40-$5F,
        // which sit outside the FastROM region ($80-$BF:$8000-$FFFF
        // and banks $C0-$FF).
        let mut plan = MapPlan::new("fast-non-fastrom-region");
        plan.push(MapSlot::new(
            0x40..=0x5F,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        let cart = PlainCartridge::new(ramp_rom(0x20 * 0x8000), 0, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));
        // The slot's declared Fast speed is honoured at install
        // because MEMSEL never touched this region.
        assert_eq!(bus.cycles_for(0x40_8000), 6);
        bus.write(0x00_420D, 0x01);
        assert_eq!(bus.cycles_for(0x40_8000), 6);
        bus.write(0x00_420D, 0x00);
        assert_eq!(bus.cycles_for(0x40_8000), 6);
    }

    #[test]
    fn hv_latched_via_4201_falling_edge() {
        let mut bus = SnesBus::new();
        // WRIO bit 7 high -> low triggers an HV latch. The bus has
        // no scheduler wired, so the latched value is (0, 0).
        bus.write(0x00_4201, 0x80);
        bus.write(0x00_4201, 0x00);
        // OPHCT / OPVCT now read 0 for both bytes.
        assert_eq!(bus.read(0x00_213C), 0);
        assert_eq!(bus.read(0x00_213C), 0);
        assert_eq!(bus.read(0x00_213D), 0);
        assert_eq!(bus.read(0x00_213D), 0);
    }

    #[test]
    fn hv_latched_via_213f_read() {
        let mut bus = SnesBus::new();
        // Reading STAT78 latches HV as a side-effect on real
        // hardware. Without a scheduler we latch (0, 0).
        let _ = bus.read(0x00_213F);
        assert_eq!(bus.read(0x00_213C), 0);
        assert_eq!(bus.read(0x00_213C), 0);
    }

    #[test]
    fn joy_data_through_bus_defaults_zero() {
        let mut bus = SnesBus::new();
        // The auto-joypad buffer ($4218-$421F) reads 0 before an
        // auto-read populates it, not MDR open-bus, because the
        // SystemRegs dispatcher returns the buffer byte.
        bus.write(0x7E_0000, 0xFF);
        let _ = bus.read(0x7E_0000); // MDR = 0xFF
        for offset in 0x4218u16..=0x421F {
            assert_eq!(bus.read(make_addr(0x00, offset)), 0);
        }
    }

    #[test]
    fn controller_manual_serial_reads_bit0_through_bus() {
        let mut bus = SnesBus::new();
        // Hold B (bit 15) on port 0.
        bus.set_controller_buttons(0, 0x8000);
        // Strobe high then low via $4016 bit 0 to latch.
        bus.write(make_addr(0x00, 0x4016), 0x01);
        bus.write(make_addr(0x00, 0x4016), 0x00);
        // First serial read returns B in bit 0; upper bits are open bus.
        assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
        // The next 15 bits are 0 (no other button held).
        for _ in 0..15 {
            assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 0);
        }
        // Past the 16 button bits the data line pulls high.
        assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
    }

    #[test]
    fn controller_serial_upper_bits_are_open_bus() {
        let mut bus = SnesBus::new();
        bus.set_controller_buttons(0, 0x8000);
        bus.write(make_addr(0x00, 0x4016), 0x01);
        bus.write(make_addr(0x00, 0x4016), 0x00);
        // Latch a known MDR (high bits) via WRAM.
        bus.write(0x7E_0000, 0xFE);
        let _ = bus.read(0x7E_0000); // MDR = 0xFE
                                     // $4016 read: bit 0 = B (1), bits 1-7 from MDR (0xFE & 0xFE).
        assert_eq!(bus.read(make_addr(0x00, 0x4016)), 0xFF);
    }

    #[test]
    fn auto_joypad_read_latches_both_ports_and_sets_busy() {
        let mut bus = SnesBus::new();
        // Port 0 = B (0x8000), port 1 = Start (0x1000).
        bus.set_controller_buttons(0, 0x8000);
        bus.set_controller_buttons(1, 0x1000);
        bus.auto_joypad_read();
        // JOY1 ($4218 low / $4219 high) = 0x8000.
        assert_eq!(bus.read(make_addr(0x00, 0x4218)), 0x00);
        assert_eq!(bus.read(make_addr(0x00, 0x4219)), 0x80);
        // JOY2 ($421A low / $421B high) = 0x1000.
        assert_eq!(bus.read(make_addr(0x00, 0x421A)), 0x00);
        assert_eq!(bus.read(make_addr(0x00, 0x421B)), 0x10);
        // JOY3/JOY4 stay 0 (no multitap).
        for offset in 0x421Cu16..=0x421F {
            assert_eq!(bus.read(make_addr(0x00, offset)), 0);
        }
        // HVBJOY ($4212) bit 0 (busy) is set; clearing it leaves the
        // other bits untouched.
        assert_eq!(bus.read(make_addr(0x00, 0x4212)) & 0x01, 0x01);
        bus.auto_joypad_finish();
        assert_eq!(bus.read(make_addr(0x00, 0x4212)) & 0x01, 0x00);
    }

    #[test]
    fn reset_controllers_clears_held_state() {
        let mut bus = SnesBus::new();
        bus.set_controller_buttons(0, 0xFFFF);
        bus.reset_controllers();
        bus.auto_joypad_read();
        assert_eq!(bus.read(make_addr(0x00, 0x4218)), 0x00);
        assert_eq!(bus.read(make_addr(0x00, 0x4219)), 0x00);
    }

    #[test]
    fn tier_dispatch_returns_same_value() {
        let mut bus = SnesBus::new();
        install_fastrom_lorom(&mut bus);
        // ROM byte at $80:8000, covers Cartridge path.
        for addr in [
            0x7E_0000u32, // Wram
            0x00_0000,    // WramMirror
            0x00_2180,    // BBus (WMDATA path)
            0x00_4216,    // CpuMmio (SystemRegs read)
            0x80_8000,    // Cartridge
            0x00_5000,    // OpenBus
        ] {
            let baseline = bus.read(addr);
            assert_eq!(bus.read_full(addr), baseline);
            assert_eq!(bus.read_light(addr), baseline);
            assert_eq!(bus.read_fast(addr), baseline);
        }

        // Mirror for writes: every tier must touch the same backing
        // store. Pick WRAM so the side-effect is observable.
        bus.write_fast(0x7E_0010, 0xAA);
        assert_eq!(bus.read(0x7E_0010), 0xAA);
        bus.write_light(0x7E_0011, 0xBB);
        assert_eq!(bus.read(0x7E_0011), 0xBB);
        bus.write_full(0x7E_0012, 0xCC);
        assert_eq!(bus.read(0x7E_0012), 0xCC);
    }

    #[test]
    fn cpu_mmio_unmapped_returns_mdr() {
        let mut bus = SnesBus::new();
        // Latch a known MDR.
        bus.write(0x7E_0000, 0xC7);
        let _ = bus.read(0x7E_0000);
        // $430C-$430E are unmapped DMA slots: writes drop, reads
        // surface MDR. Writing still latches MDR like any bus write.
        bus.write(0x00_430C, 0x42);
        assert_eq!(bus.mdr(), 0x42);
        // Refresh MDR via WRAM, then confirm the unmapped slot reads
        // the open-bus value rather than any stored byte.
        bus.write(0x7E_0001, 0x5A);
        let _ = bus.read(0x7E_0001);
        assert_eq!(bus.read(0x00_430C), 0x5A);
        // ($4016/$4017 are not open bus: the controller ports drive
        // bit 0; see the controller_* tests.)
    }

    #[test]
    fn writing_mdmaen_flags_pending_general_dma() {
        let mut bus = SnesBus::new();
        // No general DMA pending at boot.
        assert_eq!(bus.take_pending_general_dma(), 0);
        // A non-zero $420B write flags the pending mask but does NOT
        // run the transfer (that's the instruction-boundary job).
        bus.write(0x00_420B, 0x03);
        assert_eq!(bus.take_pending_general_dma(), 0x03);
        // take_* clears it.
        assert_eq!(bus.take_pending_general_dma(), 0);
        // A zero $420B write flags nothing.
        bus.write(0x00_420B, 0x00);
        assert_eq!(bus.take_pending_general_dma(), 0);
    }

    #[test]
    fn dma_channel_registers_round_trip_through_bus() {
        let mut bus = SnesBus::new();
        // Program channel 0 DMAPx + BBADx + A1Tx through the bus and
        // read them back; channel = bits 4-6 of the low byte.
        bus.write(0x00_4300, 0x01); // DMAP0: mode 1, A->B
        bus.write(0x00_4301, 0x18); // BBAD0 -> $2118 (VMDATA)
        bus.write(0x00_4302, 0x34);
        bus.write(0x00_4303, 0x12); // A1T0 = $1234
        bus.write(0x00_4304, 0x7E); // A1B0 = $7E
        assert_eq!(bus.read(0x00_4300), 0x01);
        assert_eq!(bus.read(0x00_4301), 0x18);
        assert_eq!(bus.read(0x00_4302), 0x34);
        assert_eq!(bus.read(0x00_4303), 0x12);
        assert_eq!(bus.read(0x00_4304), 0x7E);
        // Channel 7 lives at $4370-$437F.
        bus.write(0x00_437A, 0x09); // NTRL7 line counter
        assert_eq!(bus.read(0x00_437A), 0x09);
        // The controller accessor agrees with the bus dispatch.
        assert_eq!(bus.dma().channels[0].b_bus_port, 0x18);
        assert_eq!(bus.dma().channels[0].a_bus_address, 0x1234);
        assert_eq!(bus.dma().channels[7].line_counter, 0x09);
    }

    #[test]
    fn slhv_2137_latches_cached_raster() {
        let mut bus = SnesBus::new();
        bus.set_raster_snapshot(123, 45);
        // SLHV read returns A-bus MDR; we just need to make sure
        // it doesn't crash.
        bus.write(0x7E_0000, 0x99);
        let _ = bus.read(0x7E_0000);
        let r = bus.read(0x00_2137);
        assert_eq!(r, 0x99); // MDR
                             // OPHCT / OPVCT now expose (123, 45).
        assert_eq!(bus.read(0x00_213C), 123u8);
        assert_eq!(bus.read(0x00_213C), (123u16 >> 8) as u8);
        assert_eq!(bus.read(0x00_213D), 45u8);
        assert_eq!(bus.read(0x00_213D), (45u16 >> 8) as u8);
    }

    #[test]
    fn stat78_213f_consumes_counters_latched_flag() {
        let mut bus = SnesBus::new();
        // Initial read: no recent latch -> bit 6 = 1.
        let r0 = bus.read(0x00_213F);
        assert!(r0 & 0x40 != 0, "bit 6 should be 1 before any latch");
        // Strobe SLHV to set counters_latched.
        bus.set_raster_snapshot(10, 20);
        let _ = bus.read(0x00_2137);
        // First STAT78 read consumes the flag -> bit 6 = 0.
        let r1 = bus.read(0x00_213F);
        assert!(r1 & 0x40 == 0, "bit 6 should be 0 right after a latch");
        // Second read: flag was cleared on the previous read, so
        // we're back to "idle" -> bit 6 = 1.
        let r2 = bus.read(0x00_213F);
        assert!(r2 & 0x40 != 0, "bit 6 should be 1 again after consume");
    }

    #[test]
    fn stat78_213f_carries_ppu2_version_and_pal_bit() {
        let mut bus = SnesBus::new();
        // NTSC default.
        assert!(bus.read(0x00_213F) & 0x10 == 0);
        // Bits 0-3 hold PPU2 version = 1.
        let r_ntsc = bus.read(0x00_213F);
        assert_eq!(r_ntsc & 0x0F, 1);
        // Flip to PAL.
        bus.system_regs_mut().set_timing_is_pal(true);
        let r_pal = bus.read(0x00_213F);
        assert!(r_pal & 0x10 != 0, "PAL bit must be set when timing is PAL");
        assert_eq!(r_pal & 0x0F, 1);
    }

    #[test]
    fn stat78_213f_bit7_mirrors_ppu_field() {
        let mut bus = SnesBus::new();
        // Fresh PPU -> field = false -> bit 7 = 0.
        let r0 = bus.read(0x00_213F);
        assert_eq!(r0 & 0x80, 0, "field=false -> STAT78 bit 7 clear");
        // Flip the PPU field directly (the system layer drives
        // this from VBlankStart, but the bus path is what we're
        // testing here).
        bus.ppu_mut().toggle_field();
        let r1 = bus.read(0x00_213F);
        assert!(r1 & 0x80 != 0, "field=true -> STAT78 bit 7 set");
        bus.ppu_mut().toggle_field();
        let r2 = bus.read(0x00_213F);
        assert_eq!(r2 & 0x80, 0, "second toggle returns bit 7 to clear");
    }

    #[test]
    fn stat78_213f_resets_hv_toggles() {
        let mut bus = SnesBus::new();
        bus.set_raster_snapshot(0x1234, 0x0123);
        // SLHV latches.
        let _ = bus.read(0x00_2137);
        // Half-read OPHCT so the toggle moves to "high byte next".
        let lo = bus.read(0x00_213C);
        assert_eq!(lo, 0x34);
        // Read STAT78, this should reset the H/V toggles.
        let _ = bus.read(0x00_213F);
        // Next OPHCT read goes back to LOW byte instead of high.
        assert_eq!(bus.read(0x00_213C), 0x34);
        assert_eq!(bus.read(0x00_213C), 0x12);
    }

    #[test]
    fn wrio_falling_edge_uses_cached_raster() {
        let mut bus = SnesBus::new();
        bus.set_raster_snapshot(50, 100);
        // Prime $4201 high so the next write produces a falling
        // edge on bit 7.
        bus.write(0x00_4201, 0x80);
        // Now write 0 -> falling edge -> LatchHv side-effect.
        bus.write(0x00_4201, 0x00);
        // OPHCT / OPVCT now reflect the cached snapshot, not (0, 0).
        assert_eq!(bus.read(0x00_213C), 50);
        let _ = bus.read(0x00_213C); // discard high byte
        assert_eq!(bus.read(0x00_213D), 100);
    }

    #[test]
    fn ppu_accessor_round_trips() {
        let mut bus = SnesBus::new();
        // Drive INIDISP brightness via the bus dispatch.
        bus.write(0x00_2100, 0x0F);
        assert_eq!(bus.ppu().display_brightness(), 0x0F);
        assert!(!bus.ppu().display_disable());
        // Re-blank via the mutator.
        bus.ppu_mut().reset();
        assert!(bus.ppu().display_disable());
    }

    #[test]
    fn raster_snapshot_propagation() {
        let mut bus = SnesBus::new();
        assert_eq!(bus.cached_h_dot(), 0);
        assert_eq!(bus.cached_v_scanline(), 0);
        bus.set_raster_snapshot(200, 150);
        assert_eq!(bus.cached_h_dot(), 200);
        assert_eq!(bus.cached_v_scanline(), 150);
    }

    // -- Expansion-chip MMIO carve-out + $43xx observation ----------

    mod expansion {
        use crate::bus::SnesBus;
        use crate::cartridge::{
            BusSpeed, ComposableCartridge, MapPlan, MapSlot, MapTarget, PlainCartridge,
        };
        use retro_core::cartridge::{
            ChipSet, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
        };
        use std::sync::{Arc, Mutex};

        /// Shared, inspectable record of everything the console pushed at the
        /// chip. `Coprocessor` is `Send`, so the handle is `Arc<Mutex<_>>`,
        /// never `Rc`.
        #[derive(Default)]
        struct Log {
            reads: u64,
            writes: Vec<(u32, u8)>,
            observed: Vec<(u32, u8)>,
        }

        /// A minimal coprocessor: reads return a sentinel encoding the low
        /// offset nibble; writes and observed bus writes are logged so the
        /// test can prove the bus reached the chip through each path.
        struct SpyChip {
            info: ExpansionChipInfo,
            log: Arc<Mutex<Log>>,
        }

        impl Coprocessor for SpyChip {
            fn info(&self) -> &ExpansionChipInfo {
                &self.info
            }
            fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
                self.log.lock().unwrap().reads += 1;
                0xA0 | (offset as u8 & 0x0F)
            }
            fn write(&mut self, offset: u32, value: u8, _master_cycle: u64) {
                self.log.lock().unwrap().writes.push((offset, value));
            }
            fn observe_write(&mut self, addr: u32, value: u8) {
                self.log.lock().unwrap().observed.push((addr, value));
            }
            fn reset(&mut self, _master_clock_hz: u64) {}
            fn serialize(&self) -> Vec<u8> {
                Vec::new()
            }
            fn restore(&mut self, _data: &[u8], _master_clock_hz: u64) {}
            fn debug_state(&self) -> ExpansionChipState {
                ExpansionChipState {
                    chip_type: self.info.chip_type,
                    registers: Vec::new(),
                    internal_ram: None,
                    program_counter: None,
                    cycle_count: 0,
                }
            }
            fn label(&self) -> &str {
                "SPY"
            }
        }

        fn spy_chip(log: Arc<Mutex<Log>>) -> Box<SpyChip> {
            Box::new(SpyChip {
                info: ExpansionChipInfo {
                    name: "SPY".to_string(),
                    chip_type: ExpansionChipType::DSP1,
                    description: String::new(),
                    features: Vec::new(),
                },
                log,
            })
        }

        /// A composable cartridge whose one chip (id 1) both *owns* the
        /// expansion-MMIO window `$00-$3F`/`$80-$BF : $4800-$480F` (a chip
        /// window inside the CpuMmio page the walker can't promote) and
        /// *observes* the DMA registers `$4300-$437F`.
        fn cart_with_expansion_chip(log: Arc<Mutex<Log>>) -> Box<ComposableCartridge> {
            let mut plan = MapPlan::new("expansion-test");
            // Stock ROM window so the cartridge dispatches normally elsewhere.
            plan.push(MapSlot::new(
                0x80..=0xFF,
                0x8000..=0xFFFF,
                MapTarget::Rom { base_offset: 0 },
                BusSpeed::Fast,
            ));
            // Chip MMIO carved out of the CpuMmio page. The 16-byte window
            // with base 0 makes the slot-relative offset ($0-$F) reach the
            // chip register directly. Two slots cover the two CpuMmio bank
            // halves ($00-$3F and $80-$BF).
            plan.push(MapSlot::new(
                0x00..=0x3F,
                0x4800..=0x480F,
                MapTarget::ChipMmio {
                    chip_id: 1,
                    base: 0,
                },
                BusSpeed::Fast,
            ));
            plan.push(MapSlot::new(
                0x80..=0xBF,
                0x4800..=0x480F,
                MapTarget::ChipMmio {
                    chip_id: 1,
                    base: 0,
                },
                BusSpeed::Fast,
            ));
            let mut chips = ChipSet::default();
            chips.insert(1, spy_chip(log));
            chips.add_observer(0x4300..=0x437F, 1);
            Box::new(ComposableCartridge::new(
                Arc::from(vec![0u8; 0x8000].into_boxed_slice()),
                Vec::new(),
                plan,
                chips,
            ))
        }

        #[test]
        fn expansion_mmio_read_reaches_chip() {
            let log = Arc::new(Mutex::new(Log::default()));
            let mut bus = SnesBus::new();
            bus.install_cartridge(cart_with_expansion_chip(log.clone()));

            // $4800 lives on a CpuMmio page the walker left unpromoted; the
            // carve-out routes it to the chip. Sentinel = 0xA0 | (offset & F).
            assert_eq!(bus.read_full(0x00_4800), 0xA0);
            assert_eq!(bus.read_full(0x00_4805), 0xA5);
            // The $80-$BF mirror of the CpuMmio page reaches the same chip,
            // proof the carve-out follows the map plan for both bank halves.
            assert_eq!(bus.read_full(0x80_4800), 0xA0);
            assert_eq!(log.lock().unwrap().reads, 3);
        }

        #[test]
        fn expansion_mmio_write_reaches_chip() {
            let log = Arc::new(Mutex::new(Log::default()));
            let mut bus = SnesBus::new();
            bus.install_cartridge(cart_with_expansion_chip(log.clone()));

            bus.write_full(0x00_4800, 0x5A);
            bus.write_full(0x00_4803, 0x77);
            assert_eq!(log.lock().unwrap().writes, vec![(0, 0x5A), (3, 0x77)]);
        }

        #[test]
        fn dma_reg_write_is_observed_by_chip() {
            let log = Arc::new(Mutex::new(Log::default()));
            let mut bus = SnesBus::new();
            bus.install_cartridge(cart_with_expansion_chip(log.clone()));

            // $4302 = channel 0 A-bus source-address low byte, a plain latch;
            // the round-trip proves the DMA controller still owns the reg.
            bus.write_full(0x00_4302, 0x12);
            assert_eq!(bus.read_full(0x00_4302), 0x12, "DMA register still works");
            // The $80-$BF mirror hits the same 16-bit register; the observer's
            // single $4300-$437F range must still match (bank-normalized key).
            bus.write_full(0x80_4302, 0x34);

            assert_eq!(
                log.lock().unwrap().observed,
                vec![(0x4302, 0x12), (0x4302, 0x34)],
                "both bank mirrors forward the bank-agnostic 16-bit offset"
            );
        }

        #[test]
        fn plain_cartridge_does_not_regress() {
            let mut plan = MapPlan::new("plain");
            plan.push(MapSlot::new(
                0x80..=0xFF,
                0x8000..=0xFFFF,
                MapTarget::Rom { base_offset: 0 },
                BusSpeed::Fast,
            ));
            let cart = PlainCartridge::new(vec![0x11u8; 0x8000], 0, plan);
            let mut bus = SnesBus::new();
            bus.install_cartridge(Box::new(cart));

            // Carve-out disarmed -> $4800 is open bus (returns MDR = the last
            // value on the bus, here the ROM byte just read).
            let rom_byte = bus.read_full(0x80_8000);
            assert_eq!(rom_byte, 0x11);
            assert_eq!(
                bus.read_full(0x00_4800),
                rom_byte,
                "no chip claims $4800 -> open bus, not a carve-out"
            );
            // The B-bus carve-out stays disarmed too: $2200 is open bus (MDR is
            // still the ROM byte from the read just above).
            assert_eq!(
                bus.read_full(0x00_2200),
                rom_byte,
                "no chip claims $2200 -> open bus, not a carve-out"
            );
            // DMA registers untouched by the (disarmed) observe hook.
            bus.write_full(0x00_4302, 0x55);
            assert_eq!(bus.read_full(0x00_4302), 0x55);
        }

        /// A composable cartridge whose one chip (id 1) owns the B-bus MMIO
        /// window `$00-$3F`/`$80-$BF : $2200-$220F`, a chip window inside the
        /// B-bus page ($2000-$2FFF) the page-table walker can't promote. This
        /// is the SA-1 register-file seam.
        fn cart_with_bbus_chip(log: Arc<Mutex<Log>>) -> Box<ComposableCartridge> {
            let mut plan = MapPlan::new("bbus-test");
            plan.push(MapSlot::new(
                0x80..=0xFF,
                0x8000..=0xFFFF,
                MapTarget::Rom { base_offset: 0 },
                BusSpeed::Fast,
            ));
            // Chip MMIO carved out of the B-bus page, both bank halves. Base 0 +
            // 16-byte window makes the slot-relative offset ($0-$F) reach the
            // chip register directly.
            for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
                plan.push(MapSlot::new(
                    banks,
                    0x2200..=0x220F,
                    MapTarget::ChipMmio {
                        chip_id: 1,
                        base: 0,
                    },
                    BusSpeed::Slow,
                ));
            }
            let mut chips = ChipSet::default();
            chips.insert(1, spy_chip(log));
            Box::new(ComposableCartridge::new(
                Arc::from(vec![0u8; 0x8000].into_boxed_slice()),
                Vec::new(),
                plan,
                chips,
            ))
        }

        #[test]
        fn bbus_mmio_read_reaches_chip() {
            let log = Arc::new(Mutex::new(Log::default()));
            let mut bus = SnesBus::new();
            bus.install_cartridge(cart_with_bbus_chip(log.clone()));

            // $2200 lives on a BBus page the walker left unpromoted; the
            // carve-out routes it to the chip. Sentinel = 0xA0 | (offset & F).
            assert_eq!(bus.read_full(0x00_2200), 0xA0);
            assert_eq!(bus.read_full(0x00_2205), 0xA5);
            // The $80-$BF mirror of the BBus page reaches the same chip.
            assert_eq!(bus.read_full(0x80_2200), 0xA0);
            assert_eq!(log.lock().unwrap().reads, 3);
        }

        #[test]
        fn bbus_mmio_write_reaches_chip() {
            let log = Arc::new(Mutex::new(Log::default()));
            let mut bus = SnesBus::new();
            bus.install_cartridge(cart_with_bbus_chip(log.clone()));

            bus.write_full(0x00_2203, 0x5A);
            bus.write_full(0x80_2207, 0x77);
            assert_eq!(log.lock().unwrap().writes, vec![(3, 0x5A), (7, 0x77)]);
        }

        #[test]
        fn bbus_ppu_and_apu_untouched_by_carve_out() {
            // The carve-out arm sits below the $2100-$21FF (PPU) and
            // $2140-$217F (APU) arms, so a chip claiming $2200-$23FF must not
            // shadow the PPU/APU windows in the same page.
            let log = Arc::new(Mutex::new(Log::default()));
            let mut bus = SnesBus::new();
            bus.install_cartridge(cart_with_bbus_chip(log.clone()));
            // A PPU register write ($2100 INIDISP) + an APU port round-trip go
            // to their real handlers, never the spy chip.
            bus.write_full(0x00_2100, 0x0F);
            let _ = bus.read_full(0x00_2140);
            assert!(
                log.lock().unwrap().writes.is_empty(),
                "PPU/APU traffic must not reach the SA-1 register carve-out"
            );
        }

        #[test]
        fn cart_runs_burst_cpu_gate_tracks_bbus_claim() {
            // The SA-1 seam (a chip claiming $2200-$23FF) arms the
            // per-instruction burst-CPU catch-up in system.rs.
            let log = Arc::new(Mutex::new(Log::default()));
            let mut bus = SnesBus::new();
            bus.install_cartridge(cart_with_bbus_chip(log));
            assert!(
                bus.cart_runs_burst_cpu(),
                "a B-bus MMIO claimer (SA-1) runs a burst CPU"
            );

            // A plain ROM cartridge leaves the gate disarmed.
            let mut plan = MapPlan::new("plain-burst");
            plan.push(MapSlot::new(
                0x80..=0xFF,
                0x8000..=0xFFFF,
                MapTarget::Rom { base_offset: 0 },
                BusSpeed::Fast,
            ));
            let mut plain_bus = SnesBus::new();
            plain_bus.install_cartridge(Box::new(PlainCartridge::new(vec![0u8; 0x8000], 0, plan)));
            assert!(
                !plain_bus.cart_runs_burst_cpu(),
                "a plain cart keeps its current cadence"
            );

            // The Super FX opts in independently: a ChipMmio slot covering
            // $3000-$34FF on a system-area bank arms the same burst gate with no
            // B-bus carve-out (the $3xxx page is promoted, not carved).
            let mut sfx_plan = MapPlan::new("superfx-burst");
            sfx_plan.push(MapSlot::new(
                0x00..=0x3F,
                0x3000..=0x34FF,
                MapTarget::ChipMmio {
                    chip_id: 8,
                    base: 0,
                },
                BusSpeed::Slow,
            ));
            let mut sfx_bus = SnesBus::new();
            sfx_bus.install_cartridge(Box::new(PlainCartridge::new(
                vec![0u8; 0x8000],
                0,
                sfx_plan,
            )));
            assert!(
                sfx_bus.cart_runs_burst_cpu(),
                "a $3000-$34FF MMIO claimer (Super FX) runs a burst CPU"
            );

            // The Cx4 opts in too: a ChipMmio slot with the Cx4 chip_id (9)
            // covering $6000-$7FFF on a system-area bank arms the burst gate.
            let mut cx4_plan = MapPlan::new("cx4-burst");
            cx4_plan.push(MapSlot::new(
                0x00..=0x3F,
                0x6000..=0x7FFF,
                MapTarget::ChipMmio {
                    chip_id: 9,
                    base: 0,
                },
                BusSpeed::Slow,
            ));
            let mut cx4_bus = SnesBus::new();
            cx4_bus.install_cartridge(Box::new(PlainCartridge::new(
                vec![0u8; 0x8000],
                0,
                cx4_plan,
            )));
            assert!(
                cx4_bus.cart_runs_burst_cpu(),
                "a $6000-$7FFF MMIO claimer with the Cx4 chip_id runs a burst CPU"
            );

            // The Cx4 detector is chip_id-specific: an SPC7110-style SRAM slot
            // (a *different* chip_id) over the same $6000-$7FFF window must NOT
            // arm the burst gate, it is not a burst CPU.
            let mut spc_plan = MapPlan::new("spc7110-sram");
            spc_plan.push(MapSlot::new(
                0x00..=0x3F,
                0x6000..=0x7FFF,
                MapTarget::ChipMmio {
                    chip_id: 6,
                    base: 0,
                },
                BusSpeed::Slow,
            ));
            let mut spc_bus = SnesBus::new();
            spc_bus.install_cartridge(Box::new(PlainCartridge::new(
                vec![0u8; 0x8000],
                0,
                spc_plan,
            )));
            assert!(
                !spc_bus.cart_runs_burst_cpu(),
                "an SPC7110 SRAM window ($6000-$7FFF, other chip_id) is not a burst CPU"
            );
        }

        /// A coprocessor whose S-CPU IRQ line is always asserted, to prove the
        /// `has_irq()` chain (`Coprocessor` -> `ChipSet` -> `ComposableCartridge`
        /// -> `SnesBus::cart_has_irq`) propagates end to end.
        struct AlwaysIrqChip {
            info: ExpansionChipInfo,
        }

        impl Coprocessor for AlwaysIrqChip {
            fn info(&self) -> &ExpansionChipInfo {
                &self.info
            }
            fn read(&mut self, _offset: u32, _master_cycle: u64) -> u8 {
                0
            }
            fn write(&mut self, _offset: u32, _value: u8, _master_cycle: u64) {}
            fn reset(&mut self, _master_clock_hz: u64) {}
            fn serialize(&self) -> Vec<u8> {
                Vec::new()
            }
            fn restore(&mut self, _data: &[u8], _master_clock_hz: u64) {}
            fn debug_state(&self) -> ExpansionChipState {
                ExpansionChipState {
                    chip_type: self.info.chip_type,
                    registers: Vec::new(),
                    internal_ram: None,
                    program_counter: None,
                    cycle_count: 0,
                }
            }
            fn label(&self) -> &str {
                "IRQ"
            }
            fn has_irq(&self) -> bool {
                true
            }
        }

        #[test]
        fn cart_has_irq_propagates_from_chip_to_bus() {
            // An SA-1-style cart with an IRQ-asserting chip reports the line.
            let mut plan = MapPlan::new("irq-cart");
            plan.push(MapSlot::new(
                0x80..=0xFF,
                0x8000..=0xFFFF,
                MapTarget::Rom { base_offset: 0 },
                BusSpeed::Fast,
            ));
            let mut chips = ChipSet::default();
            chips.insert(
                7,
                Box::new(AlwaysIrqChip {
                    info: ExpansionChipInfo {
                        name: "IRQ".to_string(),
                        chip_type: ExpansionChipType::SA1,
                        description: String::new(),
                        features: Vec::new(),
                    },
                }),
            );
            let mut bus = SnesBus::new();
            bus.install_cartridge(Box::new(ComposableCartridge::new(
                Arc::from(vec![0u8; 0x8000].into_boxed_slice()),
                Vec::new(),
                plan,
                chips,
            )));
            assert!(bus.cart_has_irq(), "an IRQ-asserting chip raises the line");

            // A plain ROM cart never asserts the S-CPU IRQ line.
            let mut plain_plan = MapPlan::new("plain-irq");
            plain_plan.push(MapSlot::new(
                0x80..=0xFF,
                0x8000..=0xFFFF,
                MapTarget::Rom { base_offset: 0 },
                BusSpeed::Fast,
            ));
            let mut plain_bus = SnesBus::new();
            plain_bus.install_cartridge(Box::new(PlainCartridge::new(
                vec![0u8; 0x8000],
                0,
                plain_plan,
            )));
            assert!(!plain_bus.cart_has_irq(), "a plain cart never asserts IRQ");
        }
    }
}
