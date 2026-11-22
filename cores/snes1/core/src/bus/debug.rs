//! What the bus records for the debugger and its diagnostic traces: the
//! CPU's accesses and its PPU-register and APU-port writes, which the
//! watchpoints and the write events read after each instruction, and the
//! opt-in traces of the coprocessor's register stream, the
//! synchronisation registers, the program counter and one WRAM cell.

use retro_core::capture::WriteOrigin;
use retro_core::debug::{Access, RaisedEvents};

use super::{wram_offset, SnesBus};

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
pub(super) fn is_watched_io(addr: u32) -> bool {
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

/// Bounded ring buffer behind [`SnesBus::enable_io_trace`]. Keeps the
/// most recent `cap` accesses so an arbitrarily long run only retains
/// the tail leading up to the inspected frame.
#[derive(Debug, Default)]
pub(super) struct IoTraceBuf {
    entries: std::collections::VecDeque<IoAccess>,
    cap: usize,
}

/// A CPU write to a PPU register or an APU port, recorded for the
/// debugger's write events when watch/event breakpoints are armed.
///
/// The bus records these per CPU write (only while
/// [`SnesBus::debug_armed`] is set) beside the access log the watchpoints
/// read, and the emulator matches them after each instruction. DMA-engine
/// traffic uses the internal typed read/write paths and is intentionally
/// **not** recorded here, only CPU-initiated accesses are observed.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BusDebugEvent {
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
pub(super) fn classify_b_bus_write(addr: u32) -> Option<BusDebugEvent> {
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

impl SnesBus {
    /// Arm or disarm bus-level debug recording. When `armed`, the CPU's
    /// reads and writes are logged ([`Self::accesses`]) and its register
    /// and port writes recorded ([`Self::debug_events`]); disarming clears
    /// any pending records so a later session starts clean. Mirrored from
    /// the debugger's watch/event-breakpoint state once per frame by
    /// `system.rs`.
    pub fn set_debug_armed(&mut self, armed: bool) {
        self.debug_armed = armed;
        if !armed {
            self.clear_debug_events();
        }
    }

    /// Whether bus-level debug recording is currently armed.
    pub fn debug_armed(&self) -> bool {
        self.debug_armed
    }

    /// The bytes the CPU read and wrote since the last
    /// [`SnesBus::clear_debug_events`], in order.
    pub fn accesses(&self) -> &[Access] {
        self.accesses.as_slice()
    }

    /// The PPU-register and APU-port writes recorded since the last
    /// [`SnesBus::clear_debug_events`], each once: the debugger drains
    /// them into its event breakpoints.
    pub fn raised_debug_events(&mut self) -> &mut RaisedEvents<BusDebugEvent> {
        &mut self.debug_events
    }

    /// Drop the access log and the recorded writes. The emulator clears
    /// them before each measured instruction and after checking them.
    pub fn clear_debug_events(&mut self) {
        self.accesses.clear();
        self.debug_events.clear();
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
        self.refresh_diag_on();
    }

    /// Stop capturing the I/O trace and discard whatever was buffered.
    pub fn disable_io_trace(&mut self) {
        self.io_trace = None;
        self.refresh_diag_on();
    }

    /// Start capturing the instruction-PC ring buffer, retaining the
    /// most recent `cap` program counters (see [`Self::pc_trace`]). A
    /// `cap` of 0 is clamped to 1. Enabling clears any prior capture.
    pub fn enable_pc_trace(&mut self, cap: usize) {
        self.pc_trace = Some((std::collections::VecDeque::new(), cap.max(1)));
        self.refresh_diag_on();
    }

    /// Drain and return the captured PC ring buffer (oldest first),
    /// leaving capture disabled. Empty when capture was never enabled.
    pub fn take_pc_trace(&mut self) -> Vec<u32> {
        let ring = self
            .pc_trace
            .take()
            .map(|(ring, _)| ring.into_iter().collect())
            .unwrap_or_default();
        self.refresh_diag_on();
        ring
    }

    /// `true` while any diagnostic trace (I/O, PC, or address watch) is
    /// active, or a recorder of the PPU wants the processor named as the
    /// writer of its stores. `system.rs` checks this before stamping
    /// [`Self::set_trace_pc`], so the fast path pays one test of one flag
    /// when all are off.
    #[inline]
    pub fn diag_trace_enabled(&self) -> bool {
        self.diag_on
    }

    /// Recompute [`Self::diag_on`] from what is on.
    pub(super) fn refresh_diag_on(&mut self) {
        self.diag_on = self.io_trace.is_some()
            || self.pc_trace.is_some()
            || self.addr_watch.is_some()
            || self.b_bus.ppu().records_writes();
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
        self.refresh_diag_on();
    }

    /// Drain and return the captured address-watch ring buffer (oldest
    /// first), leaving the watch disabled. Empty when never enabled.
    pub fn take_addr_watch(&mut self) -> Vec<IoAccess> {
        let accesses = self
            .addr_watch
            .take()
            .map(|(_, buf)| buf.entries.into_iter().collect())
            .unwrap_or_default();
        self.refresh_diag_on();
        accesses
    }

    /// Record the `PBR:PC` of the instruction about to execute: stamped
    /// onto any [`IoAccess`] it produces, pushed into the PC ring buffer
    /// when that capture is active, and named to the PPU as the writer of
    /// the stores the instruction makes.
    #[inline]
    pub fn set_trace_pc(&mut self, pc: u32) {
        self.trace_pc = pc;
        self.b_bus.ppu_mut().set_writer(WriteOrigin::processor(pc));
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
        let accesses = self
            .io_trace
            .take()
            .map(|buf| buf.entries.into_iter().collect())
            .unwrap_or_default();
        self.refresh_diag_on();
        accesses
    }

    /// Append one access to the ring buffer, evicting the oldest entry
    /// once `cap` is reached. Only called from the CPU read/write path
    /// when capture is enabled and the address is watched.
    #[inline]
    pub(super) fn record_io_access(&mut self, is_write: bool, addr: u32, value: u8) {
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
    pub(super) fn record_addr_watch(&mut self, is_write: bool, addr: u32, value: u8) {
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
}
