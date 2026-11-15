//! Who wrote video memory, recorded live while a host asks: the origin of
//! every byte of VRAM ([`VideoMemorySources`]) and the log of the
//! transfers and of the palette and sprite-table writes each frame makes
//! ([`FrameWrites`]). The PPU's half of the debugger's palette and tile
//! events lives here too.
//!
//! ## The writer
//!
//! A byte reaches video memory through one of the PPU's three data ports
//! (`$2118/$2119` VRAM, `$2122` CGRAM, `$2104` OAM), sent by one of four
//! writers: a general DMA, an HDMA, a store by the processor, or the
//! debugger. The port cannot tell which, so whoever drives it names itself
//! first ([`WriteOrigin`], kept in the recorders): the DMA engine before
//! each byte it moves (`bus/dma.rs`), the emulator before each instruction,
//! with the instruction's address (`SnesBus::set_trace_pc`), and the
//! debugger around its writes. The names are kept up only while something
//! is armed (the processor's address only while a recorder is: the
//! debugger's watch needs no more than to tell the debugger's writes
//! apart); every byte a port takes is put down to the writer named last.
//!
//! On this core a general DMA is a [`Writer::Transfer`] and an HDMA a
//! [`Writer::LineTransfer`], each on its DMA channel; an origin's source is
//! an A-bus address. For a DMA or an HDMA transfer it is where the byte was
//! read (for an indirect HDMA channel, inside the data its table entry
//! points at); for a store by the processor, the address of the
//! instruction that made it, `PBR:PC`, exact, since the emulator names it
//! before each instruction while a recorder is armed; 0 for the debugger. A
//! transfer from the B bus to the A bus never writes video memory, so a
//! transfer's address is always where it read.
//!
//! ## What it costs
//!
//! The PPU's ports carry no test. While a recorder or a debugger watch on
//! palette colours or tiles is armed ([`Ppu::notes_port_writes`]), the bus
//! sends the writes of the three data ports through `Ppu::write_port_noted`,
//! which runs the same port code and then notes what landed; every other
//! write, and every write while nothing is armed, through the plain
//! [`Ppu::write_reg`]. The bus decides once per write of the processor
//! (`SnesBus::write_full`), once per DMA block and once per HDMA line (the
//! DMA engine's loops come in a plain and a noting instance, `bus/dma.rs`),
//! so the bytes of a transfer are tested by nobody. The live state sits
//! behind one pointer in the PPU, made while something is armed.
//!
//! Nothing here is part of a savestate. Turning a recorder on starts it
//! empty; a reset and a state load empty both, and a ROM load builds them
//! anew.
//!
//! ## The frame-write log
//!
//! [`FrameWrites::records`] holds
//! [`WORDS_PER_RECORD`](retro_core::capture::WORDS_PER_RECORD) words a
//! record, made by [`FrameWrite::pack`]. In the record's terms, `position` is the
//! dot, `target` the B-bus port and `port_to_memory` a transfer from the B
//! bus to the A bus:
//!
//! | Word | Contents |
//! |------|----------|
//! | 0 | bits 0-7 the kind ([`WriteKind`]); bits 8-10 the DMA channel (an HDMA's, for a palette or sprite-table write made by HDMA); bit 11 the transfer goes from the B bus to the A bus; bits 12-14 the transfer unit (DMAP bits 0-2); bit 15 an indirect HDMA channel; bits 16-18 for kinds 4-5, the writer, as [`Writer`] numbers it; bit 20 for kind 1, no byte of the block reached the memory its port writes |
//! | 1 | the frame, low 32 bits |
//! | 2 | bits 0-15 the line, bits 16-31 the dot on it (0-339) |
//! | 3 | where the first byte came from on the A bus; for a palette or sprite-table write, the writer's address as a [`WriteOrigin`] holds it for the byte that completed the write (an HDMA's source byte, or the address of the processor's store) |
//! | 4 | bits 0-7 the B-bus port (`$21xx`: `$18` VRAM, `$22` CGRAM, `$04` OAM, `$80` work RAM); bits 8-31 the byte count |
//! | 5 | where the destination started |
//! | 6 | where it ended, the bytes moved (kind 2) or the value written (kinds 4-5) |
//! | 7 | kind 2 from an indirect channel: the data address its entry gave; else 0 |
//!
//! Kinds ([`WriteKind`]):
//!
//! - **1, a DMA block** (a transfer): one record per channel of a `$420B`
//!   write. Word 3 is the channel's A-bus address as the block starts.
//!   Words 5 and 6 are the first and the last place the block wrote, in the
//!   destination's own unit: a VRAM word (ports `$18`/`$19`), a colour
//!   (`$22`), an OAM byte (`$04`), a work RAM byte through the WRAM port
//!   (`$80`, 17 bits); for a transfer from the B bus to the A bus, the A-bus
//!   addresses of the first and the last byte; 0 and 0 for any other port.
//!   When the port is one of those memories' and no byte reached it (the
//!   VRAM port is shut while the picture is drawn; a lone byte only waits
//!   in the CGRAM or OAM latch; the WRAM port cannot copy work RAM), bit 20
//!   is set and both words hold the place the port pointed at as the block
//!   started.
//! - **2, an HDMA unit** (a line transfer's unit): one record per channel
//!   per line it transfers. Word 3 is the address the unit's first byte
//!   was read from (for an indirect channel, inside the data its entry
//!   points at); word 5 is the first place written, as for kind 1, or 0;
//!   word 6 holds the bytes moved, the first in bits 0-7 (up to four); word
//!   7 the data address of an indirect channel.
//! - **3, HDMA set up for the frame** (a line transfer set up): one record
//!   per enabled channel when the frame starts its HDMA. Word 3 is the
//!   channel's table address on the A bus, the count is 0, words 5 and 6
//!   are 0.
//! - **4, a palette colour** (a palette store) written by HDMA or by the
//!   processor (a DMA into CGRAM is shown by its block record, and the
//!   debugger's writes are not the frame's): the count is 2 (the two
//!   halves), word 5 the colour index it landed at, word 6 the colour,
//!   BGR555.
//! - **5, a sprite-table write** (a sprite-table store) by HDMA or by the
//!   processor: the low table takes its bytes in pairs, so a record holds
//!   the pair (count 2, word 5 the first byte's address, word 6 the two
//!   bytes, the first in bits 0-7); the high table takes one byte (count
//!   1). Addresses are OAM's own, `0-$21F`.
//!
//! The line and dot are the raster position the bus held: for a general
//! DMA and for a store by the processor, where the instruction that made
//! it started; for HDMA, the HDMA event's own position (dot 276 of the
//! line before the one drawn with the unit, about dot 6 of line 0 for the
//! set-up).
//!
//! The log keeps at most [`FRAME_WRITES_CAP`] records between two takes;
//! [`FrameWrites::dropped`] counts the ones past it.

use retro_core::capture::{
    arm_recorder, CaptureRecording, FrameWrite, FrameWrites, VideoMemorySources, WriteKind,
    WriteLog, WriteOrigin, Writer,
};
use retro_core::BREAKPOINT_PARAM_ANY;

use super::{Ppu, PpuAccessCtx};

/// Bytes of VRAM, one entry each in [`VideoMemorySources`], in the byte
/// order of `VideoDebugger::get_vram` (the low half of word `n` at byte
/// `2n`).
pub const VRAM_BYTES: usize = 0x1_0000;

/// The most records the log keeps between two takes: 2 MiB of records,
/// some thirty frames of eight HDMA channels on every line.
pub const FRAME_WRITES_CAP: usize = 65_536;

/// The B-bus ports of the three memories and of the WRAM port, as offsets
/// from `$2100`.
pub mod port {
    /// `$2104` OAMDATA.
    pub const SPRITE_TABLE: u8 = 0x04;
    /// `$2118` VMDATAL.
    pub const VRAM_LOW: u8 = 0x18;
    /// `$2119` VMDATAH.
    pub const VRAM_HIGH: u8 = 0x19;
    /// `$2122` CGDATA.
    pub const PALETTE: u8 = 0x22;
    /// `$2180` WMDATA.
    pub const WORK_RAM: u8 = 0x80;
}

/// An event the PPU raises for the debugger while a breakpoint watches for
/// it; the emulator drains them and matches each against its breakpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PpuDebugEvent {
    /// A colour was written into CGRAM (the colour index).
    PaletteWrite(u8),
    /// A write into VRAM changed a byte of a tile (the tile in 4bpp tiles,
    /// the byte address divided by 32, 0-2047).
    TileChange(u16),
}

/// The most events kept between two drains: past it, more are lost (a
/// frame loop that never drains, a headless one, keeps no more than this).
const EVENT_CAP: usize = 4096;

/// The palette colours and the tiles the debugger's breakpoints watch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventWatch {
    /// One bit a colour.
    colours: [u64; 4],
    /// One bit a 4bpp tile.
    tiles: [u64; 32],
}

/// Set bit `index` of `bits`, or every bit for [`BREAKPOINT_PARAM_ANY`];
/// an index past the set watches nothing.
fn watch_bit<const N: usize>(bits: &mut [u64; N], index: u32) {
    if index == BREAKPOINT_PARAM_ANY {
        bits.fill(u64::MAX);
    } else if (index as usize) < N * 64 {
        bits[index as usize / 64] |= 1 << (index % 64);
    }
}

/// Whether bit `index` of `bits` is set.
fn bit_set<const N: usize>(bits: &[u64; N], index: usize) -> bool {
    bits.get(index / 64)
        .is_some_and(|word| word & (1 << (index % 64)) != 0)
}

impl EventWatch {
    /// Watch nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Watch colour `param` (0-255), or every colour for
    /// [`BREAKPOINT_PARAM_ANY`].
    pub fn watch_colour(&mut self, param: u32) {
        watch_bit(&mut self.colours, param);
    }

    /// Watch tile `param` (0-2047), or every tile for
    /// [`BREAKPOINT_PARAM_ANY`].
    pub fn watch_tile(&mut self, param: u32) {
        watch_bit(&mut self.tiles, param);
    }

    /// Whether colour `colour` is watched.
    pub fn colour(&self, colour: usize) -> bool {
        bit_set(&self.colours, colour)
    }

    /// Whether tile `tile` is watched.
    pub fn tile(&self, tile: usize) -> bool {
        bit_set(&self.tiles, tile)
    }
}

/// The places a DMA block or an HDMA unit wrote, from the first to the
/// last, kept while the transfer runs.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct WriteSpan {
    open: bool,
    first: Option<u32>,
    last: u32,
}

impl WriteSpan {
    /// Start a transfer's span.
    fn open(&mut self) {
        *self = Self {
            open: true,
            first: None,
            last: 0,
        };
    }

    /// The transfer wrote `place`.
    pub(crate) fn extend(&mut self, place: u32) {
        if self.open {
            self.first.get_or_insert(place);
            self.last = place;
        }
    }

    /// End the span: the first and the last place written, if any.
    fn close(&mut self) -> Option<(u32, u32)> {
        self.open = false;
        self.first.take().map(|first| (first, self.last))
    }
}

/// What a write through the OAM port stored: the first byte's address
/// (OAM's own, `0-$21F`), how many bytes (2 for a low-table pair, 1 for a
/// high-table byte), and the value (the first byte in bits 0-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SpriteTableStore {
    pub(super) first: u16,
    pub(super) count: u8,
    pub(super) value: u16,
}

/// The live recorders and the debugger's watch, boxed in
/// [`super::record::Recorders`] while any of them is armed, so the PPU itself
/// keeps one pointer for them.
#[derive(Debug)]
pub(crate) struct LiveRecorders {
    /// Who wrote each byte of VRAM, while recorded.
    sources: Option<VideoMemorySources>,
    /// The log of the frame's writes, while recorded.
    writes: Option<WriteLog>,
    /// The palette colours and tiles the debugger's events watch.
    watch: Option<EventWatch>,
    /// The events raised for the debugger and not drained yet.
    events: Vec<PpuDebugEvent>,
    /// Who writes the bytes the ports take next.
    writer: WriteOrigin,
    /// The places the transfer running now wrote.
    span: WriteSpan,
}

impl LiveRecorders {
    /// Nothing armed yet; the processor writes.
    fn new() -> Self {
        Self {
            sources: None,
            writes: None,
            watch: None,
            events: Vec::new(),
            writer: WriteOrigin::processor(0),
            span: WriteSpan::default(),
        }
    }

    /// Nothing is armed.
    fn idle(&self) -> bool {
        self.sources.is_none() && self.writes.is_none() && self.watch.is_none()
    }

    /// Keep `event` for the debugger, unless the buffer is full or the
    /// event raised just before it, with no drain between, is the same: a
    /// DMA filling a tile raises one event, not one a byte.
    fn raise(&mut self, event: PpuDebugEvent) {
        if self.events.len() < EVENT_CAP && self.events.last() != Some(&event) {
            self.events.push(event);
        }
    }

    /// Add `record` to the log, when it is recorded.
    fn log(&mut self, record: &FrameWrite) {
        if let Some(log) = self.writes.as_mut() {
            log.push(&record.pack());
        }
    }
}

impl super::record::Recorders {
    /// The live state, made when first wanted.
    fn live_mut(&mut self) -> &mut LiveRecorders {
        self.live
            .get_or_insert_with(|| Box::new(LiveRecorders::new()))
    }

    /// Drop the live state once nothing in it is armed, and recompute the
    /// two flags from what is.
    fn settle(&mut self) {
        if self.live.as_deref().is_some_and(LiveRecorders::idle) {
            self.live = None;
        }
        self.on = self.live.is_some();
        self.recording_writes = self
            .live
            .as_deref()
            .is_some_and(|live| live.sources.is_some() || live.writes.is_some());
    }
}

impl Ppu {
    /// Arm or disarm the two live recorders as `recording` asks: a recorder
    /// turned on starts empty, one turned off is dropped.
    pub(super) fn arm_write_recorders(&mut self, recording: CaptureRecording) {
        let live = self.recorders.live_mut();
        arm_recorder(&mut live.sources, recording.video_memory_sources, || {
            VideoMemorySources::new(VRAM_BYTES)
        });
        arm_recorder(&mut live.writes, recording.frame_writes, || {
            WriteLog::new(FRAME_WRITES_CAP)
        });
        self.recorders.settle();
    }

    /// Whether a live recorder is armed: the writers then name themselves.
    #[inline]
    pub fn records_writes(&self) -> bool {
        self.recorders.recording_writes
    }

    /// Whether the writes the three data ports take must go through
    /// `Self::write_port_noted`: a live recorder or a debugger watch on
    /// palette colours or tiles is armed. The bus tests this once per write
    /// of the processor, once per DMA block and once per HDMA line, never
    /// per byte of a transfer.
    #[inline]
    pub fn notes_port_writes(&self) -> bool {
        self.recorders.on
    }

    /// Who wrote each byte of VRAM, while it is recorded.
    pub fn video_memory_sources(&self) -> Option<&VideoMemorySources> {
        self.recorders.live.as_deref()?.sources.as_ref()
    }

    /// The writes recorded since the last take, the log then empty; `None`
    /// while the log is not recorded.
    pub fn take_frame_writes(&mut self) -> Option<FrameWrites> {
        self.recorders
            .live
            .as_deref_mut()?
            .writes
            .as_mut()
            .map(WriteLog::take)
    }

    /// Empty both live recorders and drop the events not drained (a reset
    /// or a state load: what they describe is gone); they stay armed.
    pub(crate) fn forget_recorded_writes(&mut self) {
        let Some(live) = self.recorders.live.as_deref_mut() else {
            return;
        };
        if let Some(sources) = live.sources.as_mut() {
            sources.clear();
        }
        if let Some(log) = live.writes.as_mut() {
            log.clear();
        }
        live.events.clear();
        live.span = WriteSpan::default();
    }

    /// Watch the palette colours and tiles `watch` names for the
    /// debugger's events, or nothing; turning the watch off drops the
    /// events not drained.
    pub fn set_event_watch(&mut self, watch: Option<EventWatch>) {
        let current = self
            .recorders
            .live
            .as_deref()
            .and_then(|live| live.watch.as_ref());
        if current == watch.as_ref() {
            return;
        }
        let live = self.recorders.live_mut();
        if watch.is_none() {
            live.events.clear();
        }
        live.watch = watch;
        self.recorders.settle();
    }

    /// Hand every event raised since the last drain to `each`, oldest
    /// first, and forget them.
    pub fn drain_debug_events(&mut self, mut each: impl FnMut(PpuDebugEvent)) {
        if let Some(live) = self.recorders.live.as_deref_mut() {
            for event in live.events.drain(..) {
                each(event);
            }
        }
    }

    /// Name who writes the bytes the ports take next (kept only while
    /// something is armed).
    #[inline]
    pub fn set_writer(&mut self, origin: WriteOrigin) {
        if let Some(live) = self.recorders.live.as_deref_mut() {
            live.writer = origin;
        }
    }

    /// The number of the frame being built, which every record carries.
    pub(crate) fn recording_frame(&self) -> u64 {
        self.recorders.frame()
    }

    /// Keep room in the log for a transfer's record, which the transfer
    /// fills when it ends, so it comes before the writes it made; the room's
    /// place in the log, or `None` when the log is full or not recorded.
    pub(crate) fn reserve_frame_write(&mut self) -> Option<usize> {
        self.recorders
            .live
            .as_deref_mut()?
            .writes
            .as_mut()?
            .reserve()
    }

    /// Fill the room kept at `at`.
    pub(crate) fn fill_frame_write(&mut self, at: usize, record: &FrameWrite) {
        if let Some(log) = self
            .recorders
            .live
            .as_deref_mut()
            .and_then(|live| live.writes.as_mut())
        {
            log.fill(at, &record.pack());
        }
    }

    /// Add a record to the log, when it is recorded.
    pub(crate) fn push_frame_write(&mut self, record: &FrameWrite) {
        if let Some(live) = self.recorders.live.as_deref_mut() {
            live.log(record);
        }
    }

    /// Start noting the places a transfer writes.
    pub(crate) fn open_write_span(&mut self) {
        if let Some(live) = self.recorders.live.as_deref_mut() {
            live.span.open();
        }
    }

    /// The transfer wrote `place` (outside video memory: work RAM through
    /// its port, or the A bus).
    pub(crate) fn extend_write_span(&mut self, place: u32) {
        if let Some(live) = self.recorders.live.as_deref_mut() {
            live.span.extend(place);
        }
    }

    /// The first and the last place the transfer wrote, if any.
    pub(crate) fn close_write_span(&mut self) -> Option<(u32, u32)> {
        self.recorders.live.as_deref_mut()?.span.close()
    }

    /// Where B-bus port `port` points in its memory: the VRAM word after
    /// VMAIN's translation (`$18`, `$19`), the colour (`$22`), the OAM byte
    /// (`$04`); `None` for every other port.
    pub(crate) fn port_place(&self, port: u8) -> Option<u32> {
        match port {
            port::VRAM_LOW | port::VRAM_HIGH => Some(u32::from(self.vram_word_address())),
            port::PALETTE => Some(u32::from(self.cgram_addr >> 1)),
            port::SPRITE_TABLE => Some(u32::from(self.oam_addr)),
            _ => None,
        }
    }

    /// A write to one of the three data ports (`reg` `$04`, `$18`, `$19` or
    /// `$22`) that the live recorders or a debugger watch must see: the
    /// port's own code, then a note of what it took, put down to the writer
    /// named last. The bus sends the data ports' writes here while
    /// [`Self::notes_port_writes`] holds, and none otherwise; every other
    /// register goes through [`Self::write_reg`] as always, so that stays
    /// the one way into the register file and keeps its single caller.
    #[cold]
    #[inline(never)]
    pub(crate) fn write_port_noted(&mut self, reg: u8, value: u8, ctx: &PpuAccessCtx) {
        match reg & 0x3F {
            port::SPRITE_TABLE => self.write_2104_noted(value, ctx),
            port::VRAM_LOW => self.vram_data_write_noted(false, value, ctx),
            port::VRAM_HIGH => self.vram_data_write_noted(true, value, ctx),
            port::PALETTE => self.write_2122_noted(value, ctx),
            _ => unreachable!("only the three data ports are noted"),
        }
    }

    /// The VRAM port's data write, noted: what landed is put down to the
    /// writer, and a watched tile it changed raises an event.
    fn vram_data_write_noted(&mut self, high: bool, value: u8, ctx: &PpuAccessCtx) {
        let Some((byte_address, replaced)) = self.vram_port_write(high, value, ctx) else {
            return;
        };
        let byte = byte_address as usize % VRAM_BYTES;
        let frame = self.recorders.frame();
        let Some(live) = self.recorders.live.as_deref_mut() else {
            return;
        };
        let origin = live.writer;
        if let Some(sources) = live.sources.as_mut() {
            sources.note(byte, origin, frame);
        }
        live.span.extend((byte >> 1) as u32);
        // Writing the same byte again changes no tile.
        if replaced != value && origin.writer() != Writer::Debugger as u32 {
            let tile = byte >> 5;
            if live.watch.as_ref().is_some_and(|watch| watch.tile(tile)) {
                live.raise(PpuDebugEvent::TileChange(tile as u16));
            }
        }
    }

    /// The CGRAM port's data write, noted: a colour it completed is logged
    /// (by HDMA or the processor) and raises a watched colour's event.
    fn write_2122_noted(&mut self, value: u8, ctx: &PpuAccessCtx) {
        let Some(colour) = self.cgram_port_write(value, ctx) else {
            return;
        };
        let stored = self.cgram[colour];
        let frame = self.recorders.frame();
        let Some(live) = self.recorders.live.as_deref_mut() else {
            return;
        };
        let origin = live.writer;
        live.span.extend(colour as u32);
        let who = Writer::from_code(origin.writer());
        if matches!(who, Some(Writer::LineTransfer | Writer::Processor)) {
            live.log(&FrameWrite {
                kind: WriteKind::PaletteStore as u8,
                channel: if who == Some(Writer::LineTransfer) {
                    origin.channel() as u8
                } else {
                    0
                },
                writer: origin.writer() as u8,
                frame,
                line: ctx.v_scanline,
                position: ctx.h_dot,
                source: origin.source(),
                target: port::PALETTE,
                count: 2,
                start: colour as u32,
                end_or_value: u32::from(stored),
                ..FrameWrite::default()
            });
        }
        if who != Some(Writer::Debugger)
            && live
                .watch
                .as_ref()
                .is_some_and(|watch| watch.colour(colour))
        {
            live.raise(PpuDebugEvent::PaletteWrite(colour as u8));
        }
    }

    /// The OAM port's data write, noted: what it stored is logged (by HDMA
    /// or the processor).
    fn write_2104_noted(&mut self, value: u8, ctx: &PpuAccessCtx) {
        let Some(stored) = self.oam_port_write(value, ctx) else {
            return;
        };
        let frame = self.recorders.frame();
        let Some(live) = self.recorders.live.as_deref_mut() else {
            return;
        };
        let origin = live.writer;
        for offset in 0..u16::from(stored.count) {
            live.span.extend(u32::from(stored.first + offset));
        }
        let who = Writer::from_code(origin.writer());
        if matches!(who, Some(Writer::LineTransfer | Writer::Processor)) {
            live.log(&FrameWrite {
                kind: WriteKind::SpriteTableStore as u8,
                channel: if who == Some(Writer::LineTransfer) {
                    origin.channel() as u8
                } else {
                    0
                },
                writer: origin.writer() as u8,
                frame,
                line: ctx.v_scanline,
                position: ctx.h_dot,
                source: origin.source(),
                target: port::SPRITE_TABLE,
                count: u32::from(stored.count),
                start: u32::from(stored.first),
                end_or_value: u32::from(stored.value),
                ..FrameWrite::default()
            });
        }
    }

    /// The debugger wrote VRAM byte `byte` straight in: its writer is the
    /// debugger's.
    pub(super) fn note_vram_debug_write(&mut self, byte: usize) {
        let frame = self.recorders.frame();
        if let Some(sources) = self
            .recorders
            .live
            .as_deref_mut()
            .and_then(|live| live.sources.as_mut())
        {
            sources.note(byte, WriteOrigin::DEBUGGER, frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::capture::WORDS_PER_RECORD;

    /// A PPU recording its frame writes.
    fn recording_writes() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.set_capture_recording(CaptureRecording {
            frame_writes: true,
            ..CaptureRecording::default()
        });
        ppu
    }

    #[test]
    fn the_log_keeps_its_cap_counts_the_rest_and_empties_on_a_take() {
        let mut ppu = recording_writes();
        let record = FrameWrite {
            kind: WriteKind::PaletteStore as u8,
            ..FrameWrite::default()
        };
        for _ in 0..FRAME_WRITES_CAP + 3 {
            ppu.push_frame_write(&record);
        }
        let taken = ppu.take_frame_writes().expect("recorded");
        assert_eq!(taken.records.len(), FRAME_WRITES_CAP * WORDS_PER_RECORD);
        assert_eq!(taken.dropped, 3);
        let again = ppu.take_frame_writes().expect("recorded");
        assert!(again.records.is_empty());
        assert_eq!(again.dropped, 0);
    }

    #[test]
    fn a_reserved_record_keeps_its_place_before_the_ones_pushed_after() {
        let mut ppu = recording_writes();
        let at = ppu.reserve_frame_write().expect("room");
        ppu.push_frame_write(&FrameWrite {
            kind: WriteKind::PaletteStore as u8,
            ..FrameWrite::default()
        });
        ppu.fill_frame_write(
            at,
            &FrameWrite {
                kind: WriteKind::LineTransferUnit as u8,
                ..FrameWrite::default()
            },
        );
        let kinds: Vec<u8> = ppu
            .take_frame_writes()
            .expect("recorded")
            .iter()
            .map(|record| record.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                WriteKind::LineTransferUnit as u8,
                WriteKind::PaletteStore as u8
            ]
        );
    }

    #[test]
    fn a_watch_takes_one_place_every_place_or_none() {
        let mut watch = EventWatch::new();
        watch.watch_tile(2047);
        watch.watch_colour(3);
        assert!(watch.tile(2047) && !watch.tile(2046));
        assert!(watch.colour(3) && !watch.colour(4));
        watch.watch_tile(2048);
        assert!(!watch.tile(2048), "past the tiles watches nothing");
        watch.watch_colour(BREAKPOINT_PARAM_ANY);
        assert!((0..256).all(|colour| watch.colour(colour)));
    }

    #[test]
    fn a_span_holds_its_first_and_last_place_while_open() {
        let mut span = WriteSpan::default();
        span.extend(9);
        span.open();
        assert_eq!(span.close(), None, "nothing written");
        span.open();
        for place in [5, 3, 8] {
            span.extend(place);
        }
        assert_eq!(span.close(), Some((5, 8)));
        span.extend(1);
        assert_eq!(span.close(), None, "a closed span notes nothing");
    }
}
