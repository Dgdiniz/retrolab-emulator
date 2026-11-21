//! Who wrote video memory, and what each frame wrote: the two recorders
//! that run while frames run, while a host asks for them, and the
//! debugger's event breakpoints on the same writes.
//!
//! - [`Vdp::video_memory_sources`] keeps, for each byte of VRAM, what wrote
//!   it last and from where: a transfer from 68000 memory (the exact source
//!   address of the byte), a store by the 68000 through the data port, a
//!   fill, a copy within VRAM, or the debugger. Each byte also keeps the
//!   frame it was last written in.
//! - [`Vdp::take_frame_writes`] gives the transfers, fills and copies, and
//!   the palette and sprite-table stores made through the data port, since
//!   the last take, eight words a record. Taken at each frame's capture
//!   boundary, they are what built that frame: the uploads of the vertical
//!   blank before it and the writes made while its picture was drawn.
//! - [`Vdp::set_event_watch`] arms the debugger's event breakpoints on the
//!   same writes: a transfer finishing, a palette colour written, a
//!   transfer into the sprite table, a tile rewritten. The VDP raises only
//!   what the watch asks for, and the debugger drains what was raised after
//!   each instruction ([`Vdp::raised_events_mut`]).
//!
//! Every write path has one hook, behind the check of `Vdp::recording`
//! that the line table already pays: with nothing recorded a write costs
//! that one check, the bus one store of where a port write's value came
//! from ([`Vdp::note_port_origin`]), and nothing runs per instruction.
//! Nothing here is in a savestate, and recording changes nothing the
//! machine does.
//!
//! The shapes, codes and layouts are retro-core's ([`VideoMemorySources`],
//! [`WriteOrigin`], [`Writer`], [`FrameWrite`], [`WriteKind`]); what this
//! core puts in them follows.
//!
//! An origin's source (bits 0-23), by writer. The channel (bits 24-26) is
//! always 0: the VDP has a single transfer channel, and no line-by-line
//! transfer.
//!
//! - [`Writer::Transfer`], a transfer from 68000 memory: the 68000 address
//!   the byte was read from, byte by byte, the transfer's 128 KB source
//!   window wrapping as the transfer wrapped it.
//! - [`Writer::Processor`], a store by the 68000 through the data port: the
//!   address of the last word the 68000 read before the store (the bus keeps
//!   it for the status register's undriven bits). For `MOVE.W (An)+,(port)`
//!   that is the word copied; a long store names its second word for both
//!   halves; a register, an immediate or a store to an absolute port address
//!   names the last word of the storing instruction itself (the immediate,
//!   or the address). It is a pointer to where the value came from, exact
//!   for a copy from memory, and a hint otherwise: it is not the program
//!   counter.
//! - [`Writer::Debugger`] (`write_vram_debug`): 0.
//! - [`Writer::Fill`]: the byte the fill wrote.
//! - [`Writer::Copy`], a copy within VRAM: the index, in the order
//!   [`Vdp::get_vram`] gives VRAM, of the byte copied from. The copy's source
//!   register counts bytes the other way round in each word, so that index
//!   is the register's address with bit 0 flipped.
//!
//! A record of the frame writes, word by word:
//!
//! | Word | Contents |
//! |---|---|
//! | 0 | the kind: [`WriteKind::Transfer`] (a transfer from 68000 memory into VRAM, CRAM or VSRAM; one into CRAM or the sprite table is this one record, not one for each colour or entry), [`WriteKind::PaletteStore`] (a colour the 68000 stored through the data port), [`WriteKind::SpriteTableStore`] (a word the 68000 stored into the sprite table through the data port), [`WriteKind::Fill`], [`WriteKind::Copy`]; for kinds 4 and 5, the writer, always [`Writer::Processor`] |
//! | 1 | the frame, numbered as the origins' frames are |
//! | 2 | the line the beam was on; as the position, the 68000 cycles into that line when the writing instruction began, what the line's start took counted (the cycles the line before ran past its end, an interrupt's entry, a transfer's hold) |
//! | 3 | a transfer: the 68000 address of its first byte; a fill: the value its cells wrote (a byte into VRAM, a word into CRAM or VSRAM); a copy: the VRAM address it copied from first, as its source register holds it; a store (kinds 4-5): its origin's source, as [`Writer::Processor`] describes it |
//! | 4 | the memory written ([`WriteTarget`]) and the bytes written |
//! | 5 | where the writes started: a VRAM byte address as the VDP's address register holds it (bit 0 set swaps a word's bytes), a CRAM colour index, a VSRAM word index, or, for kind 5, the byte's offset in the sprite table |
//! | 6 | kinds 1, 6 and 7: where the last write landed, as word 5 counts; kind 4: the colour word stored (`0000 BBB0 GGG0 RRR0`); kind 5: the word the table holds there after the store, its high byte the one at the even address |
//! | 7 | 0 |
//!
//! A fill is recorded when its last cell is written, with the frame, line
//! and cycle of the data-port store that started it; its first cell is that
//! store's whole word, and the record counts it. A transfer and a copy land
//! at once, so each is recorded when it runs.

use retro_core::capture::{
    arm_recorder, CaptureRecording, FrameWrite, FrameWrites, VideoMemorySources, WriteKind,
    WriteLog, WriteOrigin, Writer, WORDS_PER_RECORD,
};
use retro_core::debug::{ParamSet, RaisedEvents};

use super::recording::{PaletteRedraw, Recording};
use super::Vdp;

/// The most records a take holds. Past it a record is counted in
/// [`FrameWrites::dropped`] instead: a program that writes the palette
/// for a raster effect writes about 6,300 colours a frame, so this holds
/// more than five such frames.
pub const MOST_RECORDS: usize = 32_768;

/// The bytes of VRAM.
const VRAM_BYTES: usize = 0x1_0000;
/// The bytes of a tile in VRAM, as the tile viewer numbers tiles.
const TILE_BYTES: usize = 32;
/// The tiles of VRAM.
const TILES: usize = VRAM_BYTES / TILE_BYTES;
/// The most events kept between two drains by the debugger; past it a
/// raised event is counted, not kept. One is enough to stop the debugger.
const MOST_RAISED_EVENTS: usize = 4096;

/// The memory a record wrote: bits 0-7 of its word 4, and the param of the
/// transfer-finished event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteTarget {
    /// Video memory: tiles, maps, the sprite table, the scroll table.
    Vram = 0,
    /// The palette.
    Cram = 1,
    /// The vertical scroll memory.
    Vsram = 2,
}

impl WriteTarget {
    /// The memory a VDP access code's low four bits (CD3-CD0) write, if any.
    fn of_access_code(code: u8) -> Option<Self> {
        match code & 0x0F {
            0x01 => Some(Self::Vram),
            0x03 => Some(Self::Cram),
            0x05 => Some(Self::Vsram),
            _ => None,
        }
    }

    /// Where an address in this memory is in a record: a VRAM byte
    /// address as the VDP's address register holds it, or the CRAM colour
    /// or VSRAM word the address selects.
    fn place(self, address: u16) -> u32 {
        match self {
            Self::Vram => u32::from(address),
            Self::Cram | Self::Vsram => u32::from((address & 0x7E) >> 1),
        }
    }
}

/// An event a debugger breakpoint can stop on, raised by a write to video
/// memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoEvent {
    /// A transfer from 68000 memory, a fill or a copy has written all its
    /// data into the memory named.
    TransferComplete(WriteTarget),
    /// A colour of CRAM was written: by the data port, a transfer or a
    /// fill. The colour, 0-63.
    PaletteWrite(u8),
    /// A transfer, a fill or a copy wrote into the sprite table.
    SpriteTableTransfer,
    /// A write changed a byte of the tile, 0-2047 (32 bytes each, as the
    /// tile viewer numbers them). Writing the bytes a tile already holds
    /// does not count.
    TileRewritten(u16),
}

/// The events a debugger's breakpoints ask the VDP to raise
/// ([`Vdp::set_event_watch`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoEventWatch {
    /// The memories whose transfers raise their end, each its
    /// [`WriteTarget`] code.
    pub transfer_targets: ParamSet<1>,
    /// The colours whose writes raise an event.
    pub palette_colours: ParamSet<1>,
    /// A transfer into the sprite table raises an event.
    pub sprite_table: bool,
    /// The tiles whose rewriting raises an event.
    pub tiles: ParamSet<{ TILES / 64 }>,
}

impl VideoEventWatch {
    /// Whether the watch asks for nothing.
    pub fn is_empty(&self) -> bool {
        self.transfer_targets.is_empty()
            && self.palette_colours.is_empty()
            && !self.sprite_table
            && self.tiles.is_empty()
    }

    fn watches_tiles(&self) -> bool {
        !self.tiles.is_empty()
    }

    fn watches(&self, event: VideoEvent) -> bool {
        match event {
            VideoEvent::TransferComplete(target) => self.transfer_targets.contains(target as u32),
            VideoEvent::PaletteWrite(colour) => self.palette_colours.contains(u32::from(colour)),
            VideoEvent::SpriteTableTransfer => self.sprite_table,
            VideoEvent::TileRewritten(tile) => self.tiles.contains(u32::from(tile)),
        }
    }
}

/// The debugger's watch and what it raised.
pub(super) struct EventState {
    watch: VideoEventWatch,
    raised: RaisedEvents<VideoEvent>,
}

impl EventState {
    fn raise(&mut self, event: VideoEvent) {
        if self.watch.watches(event) {
            self.raised.raise(event);
        }
    }
}

/// The frame a write was made in and where the beam was: words 1 and 2 of
/// a record.
#[derive(Clone, Copy)]
struct WriteStamp {
    /// The frame being drawn.
    frame: u64,
    /// The line the beam was on.
    line: u16,
    /// The 68000 cycles into that line.
    position: u16,
}

/// A fill from the data-port store that started it to its last cell.
struct FillInProgress {
    stamp: WriteStamp,
    target: WriteTarget,
    value: u32,
    start: u32,
    last: u32,
    bytes: u32,
    sprite_table: bool,
}

/// What the two live recorders and the event watch keep, in
/// `Recording::live`. Nothing of it is allocated until something is armed.
#[derive(Default)]
pub(super) struct LiveWrites {
    /// Who wrote each byte of VRAM, while video memory sources are armed.
    pub(super) memory: Option<VideoMemorySources>,
    /// The records since the last take, while frame writes are armed.
    pub(super) log: Option<WriteLog>,
    /// What the debugger's event breakpoints watch, and what was raised.
    pub(super) events: Option<EventState>,
    /// The fill running, while it runs.
    fill: Option<FillInProgress>,
    /// The VRAM bytes a transfer or a copy is about to write and what each
    /// held, while a tile is watched: the transfer's end compares them.
    before: Vec<(u16, u8)>,
}

impl LiveWrites {
    /// Arm or disarm the two recorders as `armed` asks: one turned on
    /// starts empty, one already on keeps what it holds.
    pub(super) fn arm(&mut self, armed: CaptureRecording) {
        arm_recorder(&mut self.memory, armed.video_memory_sources, || {
            VideoMemorySources::new(VRAM_BYTES)
        });
        arm_recorder(&mut self.log, armed.frame_writes, || {
            WriteLog::new(MOST_RECORDS)
        });
        if self.log.is_none() && self.events.is_none() {
            self.fill = None;
        }
    }

    /// Whether the debugger's event breakpoints watch anything.
    pub(super) fn watching(&self) -> bool {
        self.events.is_some()
    }

    /// Forget what was recorded, keeping what is armed: the machine changed
    /// under it (a reset, a savestate load).
    pub(super) fn clear(&mut self) {
        if let Some(memory) = &mut self.memory {
            memory.clear();
        }
        if let Some(log) = &mut self.log {
            log.clear();
        }
        if let Some(events) = &mut self.events {
            events.raised.clear();
        }
        self.fill = None;
        self.before.clear();
    }

    fn records_anything(&self) -> bool {
        self.memory.is_some() || self.log.is_some() || self.events.is_some()
    }

    fn watches_tiles(&self) -> bool {
        self.events
            .as_ref()
            .is_some_and(|events| events.watch.watches_tiles())
    }

    fn set_origin(&mut self, index: usize, origin: WriteOrigin, frame: u64) {
        if let Some(memory) = &mut self.memory {
            memory.note(index, origin, frame);
        }
    }

    fn raise(&mut self, event: VideoEvent) {
        if let Some(events) = &mut self.events {
            events.raise(event);
        }
    }

    fn raise_tile_if_changed(&mut self, index: usize, before: u8, after: u8) {
        if before != after {
            self.raise(VideoEvent::TileRewritten((index / TILE_BYTES) as u16));
        }
    }

    fn log(&mut self, record: [u32; WORDS_PER_RECORD]) {
        if let Some(log) = &mut self.log {
            log.push(&record);
        }
    }

    /// Note the bytes about to be written at `indices`, with what each
    /// holds, while a tile is watched.
    fn keep_before(&mut self, vram: &[u8], indices: impl Iterator<Item = usize>) {
        self.before.clear();
        if self.watches_tiles() {
            self.before
                .extend(indices.map(|index| (index as u16, vram[index])));
        }
    }

    /// Raise the tiles whose bytes noted by [`Self::keep_before`] changed.
    fn raise_rewritten_tiles(&mut self, vram: &[u8]) {
        let mut before = std::mem::take(&mut self.before);
        for &(index, value) in &before {
            let index = usize::from(index);
            self.raise_tile_if_changed(index, value, vram[index]);
        }
        before.clear();
        self.before = before;
    }
}

/// A record of the frame writes, packed: `kind`, made at `stamp`, from
/// `source`, of `bytes` bytes into `target` from `start` to `end` (or, for
/// a store, the value it stored). A store's writer is the 68000.
fn record(
    kind: WriteKind,
    stamp: WriteStamp,
    source: u32,
    target: WriteTarget,
    bytes: u32,
    start: u32,
    end: u32,
) -> [u32; WORDS_PER_RECORD] {
    let writer = match kind {
        WriteKind::PaletteStore | WriteKind::SpriteTableStore => Writer::Processor,
        _ => Writer::None,
    };
    FrameWrite {
        kind: kind as u8,
        writer: writer as u8,
        frame: stamp.frame,
        line: stamp.line,
        position: stamp.position,
        source,
        target: target as u8,
        count: bytes,
        start,
        end_or_value: end,
        ..FrameWrite::default()
    }
    .pack()
}

/// A transfer from 68000 memory as [`Vdp::execute_pending_dma`] ran it, for
/// the recorders.
#[derive(Clone, Copy)]
pub(super) struct MemoryTransfer {
    /// The 68000 address of the first byte.
    pub(super) source: u32,
    /// The VDP's address register when it began.
    pub(super) destination: u16,
    /// The auto-increment (register 15).
    pub(super) step: u16,
    /// The words moved.
    pub(super) words: usize,
    /// The access code (CD3-CD0 name the memory written).
    pub(super) code: u8,
}

impl MemoryTransfer {
    /// The 68000 address word `index` of the transfer was read from: the
    /// source advances inside its 128 KB window, whose base the transfer
    /// never moves, as the bus reads it.
    fn source_of_word(&self, index: usize) -> u32 {
        let window = self.source & 0x00FE_0000;
        let first = (self.source & 0x1_FFFF) >> 1;
        window | ((first.wrapping_add(index as u32) & 0xFFFF) << 1)
    }

    /// The address register's value as word `index` was written.
    fn destination_of_word(&self, index: usize) -> u16 {
        self.destination
            .wrapping_add((index as u16).wrapping_mul(self.step))
    }
}

impl Vdp {
    /// Arm the debugger's event breakpoints on writes to video memory: the
    /// VDP raises the events `watch` asks for as the writes are made, and
    /// none with `None`. A watch armed or changed starts with nothing
    /// raised. The system arms it from the breakpoint list as each debugger
    /// frame and each step begins, and clears it for the loop a player
    /// runs, so a game with no event breakpoint raises nothing.
    pub fn set_event_watch(&mut self, watch: Option<VideoEventWatch>) {
        match watch.filter(|watch| !watch.is_empty()) {
            Some(watch) => {
                let recording = self
                    .recording
                    .get_or_insert_with(|| Box::new(Recording::new(CaptureRecording::default())));
                match &mut recording.live.events {
                    Some(events) if events.watch == watch => {}
                    events => {
                        *events = Some(EventState {
                            watch,
                            raised: RaisedEvents::new(MOST_RAISED_EVENTS),
                        })
                    }
                }
            }
            None => {
                let Some(recording) = self.recording.as_deref_mut() else {
                    return;
                };
                if recording.live.events.is_none() {
                    return;
                }
                recording.live.events = None;
                recording.live.arm(recording.armed);
                if recording.armed == CaptureRecording::default() {
                    self.recording = None;
                }
            }
        }
    }

    /// The events raised since the debugger last drained them, while its
    /// event breakpoints watch something ([`Self::set_event_watch`]), else
    /// `None`. The debugger drains them after each instruction.
    #[inline(always)]
    pub fn raised_events_mut(&mut self) -> Option<&mut RaisedEvents<VideoEvent>> {
        self.recording
            .as_deref_mut()
            .and_then(|recording| recording.live.events.as_mut())
            .map(|events| &mut events.raised)
    }

    /// Who wrote each byte of VRAM and in which frame, while video memory
    /// sources are recorded ([`CaptureRecording::video_memory_sources`]),
    /// else `None`: 65,536 entries, in the order [`Self::get_vram`] gives
    /// VRAM (the high byte of each word at its even address), each frame
    /// numbered as each frame's records number it. A byte written before
    /// the first frame began reads frame 0; its origin names its writer.
    /// Turned on, the recorder starts with every byte not written; a reset,
    /// a savestate load and a ROM load empty it again.
    pub fn video_memory_sources(&self) -> Option<&VideoMemorySources> {
        self.recording.as_deref()?.live.memory.as_ref()
    }

    /// The writes made since the last take, while frame writes are recorded
    /// ([`CaptureRecording::frame_writes`]), else `None`; the record is
    /// empty again after. At most [`MOST_RECORDS`] records a take, the rest
    /// counted in [`FrameWrites::dropped`]. Turned on, it starts empty; a
    /// reset, a savestate load and a ROM load empty it again.
    pub fn take_frame_writes(&mut self) -> Option<FrameWrites> {
        Some(self.recording.as_deref_mut()?.live.log.as_mut()?.take())
    }

    /// Note where the value of the store about to be made through a VDP
    /// port came from, the address of the last word the 68000 read
    /// ([`Writer::Processor`]). The bus calls this before each
    /// write to a VDP port: one store, no check, read only by a hook that
    /// records.
    #[inline(always)]
    pub fn note_port_origin(&mut self, address: u32) {
        self.port_origin = address;
    }

    /// The frame being drawn and where the beam is on it, for a record or
    /// an origin: the beam's count, which the line's start sets and each
    /// instruction moves on.
    fn write_stamp(&self) -> WriteStamp {
        WriteStamp {
            frame: self.frame_being_drawn,
            line: self.timing.scanline,
            position: self.beam_cycles_into_line.min(0xFFFF) as u16,
        }
    }

    fn live_writes(&mut self) -> Option<&mut LiveWrites> {
        self.recording
            .as_deref_mut()
            .map(|recording| &mut recording.live)
    }

    /// Whether either live recorder or the event watch is armed: with only
    /// the line table armed, a hook journals and goes no further.
    fn records_live(&self) -> bool {
        self.recording
            .as_deref()
            .is_some_and(|recording| recording.live.records_anything())
    }

    /// Note a word the 68000 is about to store in VRAM through the data
    /// port: `raw` is the address register (bit 0 set swaps the word's
    /// bytes), `value` the word. `starts_fill` marks the store that starts
    /// a fill, whose record covers it.
    #[inline(always)]
    pub(super) fn note_port_vram_store(&mut self, raw: usize, value: u16, starts_fill: bool) {
        if self.recording.is_some() {
            self.record_port_vram_store(raw, value, starts_fill);
        }
    }

    #[inline(never)]
    fn record_port_vram_store(&mut self, raw: usize, value: u16, starts_fill: bool) {
        let address = raw & 0xFFFE;
        self.journal_vram_write(address);
        if !self.records_live() {
            return;
        }
        let stored = if raw & 1 == 0 {
            value
        } else {
            value.swap_bytes()
        };
        let before = [self.vram[address], self.vram[address + 1]];
        let stamp = self.write_stamp();
        let table = self.sprite_table_bytes();
        let origin = WriteOrigin::processor(self.port_origin);
        let Some(live) = self.live_writes() else {
            return;
        };
        let after = stored.to_be_bytes();
        for (offset, (&was, &now)) in before.iter().zip(&after).enumerate() {
            live.set_origin(address + offset, origin, stamp.frame);
            live.raise_tile_if_changed(address + offset, was, now);
        }
        if !starts_fill && table.contains(&address) {
            live.log(record(
                WriteKind::SpriteTableStore,
                stamp,
                origin.source(),
                WriteTarget::Vram,
                2,
                (address - table.start) as u32,
                u32::from(stored),
            ));
        }
    }

    /// Note a palette colour written: colour `index` became `colour`, and
    /// the write coloured again what `redraw` says of the line the beam is
    /// on; `store` says how the colour was written.
    #[inline(always)]
    pub(super) fn note_palette_change(
        &mut self,
        index: usize,
        colour: u16,
        redraw: PaletteRedraw,
        store: PaletteStore,
    ) {
        if self.recording.is_some() {
            self.record_palette_change(index, colour, redraw, store);
        }
    }

    #[inline(never)]
    fn record_palette_change(
        &mut self,
        index: usize,
        colour: u16,
        redraw: PaletteRedraw,
        store: PaletteStore,
    ) {
        self.record_line_palette_change(index, colour, redraw);
        if store == PaletteStore::Transfer || !self.records_live() {
            return;
        }
        let stamp = self.write_stamp();
        let origin = WriteOrigin::processor(self.port_origin).source();
        let Some(live) = self.live_writes() else {
            return;
        };
        live.raise(VideoEvent::PaletteWrite(index as u8));
        match store {
            PaletteStore::Port => {
                live.log(record(
                    WriteKind::PaletteStore,
                    stamp,
                    origin,
                    WriteTarget::Cram,
                    2,
                    index as u32,
                    u32::from(colour),
                ));
            }
            PaletteStore::FillCell => {
                if let Some(fill) = &mut live.fill {
                    fill.last = index as u32;
                    fill.bytes += 2;
                }
            }
            PaletteStore::FillStart | PaletteStore::Transfer => {}
        }
    }

    /// Note that a fill starts: the data-port store at `start` (the
    /// address register as the store found it) has just been written, and
    /// the fill's cells follow it.
    #[inline(always)]
    pub(super) fn note_fill_start(&mut self, start: u16) {
        if self.recording.is_some() {
            self.record_fill_start(start);
        }
    }

    #[inline(never)]
    fn record_fill_start(&mut self, start: u16) {
        let Some(target) = WriteTarget::of_access_code(self.code) else {
            return;
        };
        if !self.records_live() {
            return;
        }
        let (byte, cram_word, vsram_word) = self.fill_source_words_live();
        let value = match target {
            WriteTarget::Vram => u32::from(byte),
            WriteTarget::Cram => u32::from(cram_word),
            WriteTarget::Vsram => u32::from(vsram_word),
        };
        let stamp = self.write_stamp();
        let sprite_table = target == WriteTarget::Vram
            && self
                .sprite_table_bytes()
                .contains(&usize::from(start & 0xFFFE));
        let Some(live) = self.live_writes() else {
            return;
        };
        live.fill = (live.log.is_some() || live.events.is_some()).then_some(FillInProgress {
            stamp,
            target,
            value,
            start: target.place(start),
            last: target.place(start),
            bytes: 2,
            sprite_table,
        });
    }

    /// Note a fill cell about to write `byte` into VRAM at the address
    /// register's value `raw`: the byte lands at `raw ^ 1` in the order
    /// [`Self::get_vram`] gives VRAM.
    #[inline(always)]
    pub(super) fn note_fill_vram_cell(&mut self, raw: usize, byte: u8) {
        if self.recording.is_some() {
            self.record_fill_vram_cell(raw, byte);
        }
    }

    #[inline(never)]
    fn record_fill_vram_cell(&mut self, raw: usize, byte: u8) {
        self.journal_vram_write(raw & 0xFFFE);
        if !self.records_live() {
            return;
        }
        let index = (raw ^ 1) & (VRAM_BYTES - 1);
        let before = self.vram[index];
        let frame = self.frame_being_drawn;
        let in_table = self.sprite_table_bytes().contains(&index);
        let Some(live) = self.live_writes() else {
            return;
        };
        live.set_origin(
            index,
            WriteOrigin::new(Writer::Fill, 0, u32::from(byte)),
            frame,
        );
        live.raise_tile_if_changed(index, before, byte);
        if let Some(fill) = &mut live.fill {
            fill.last = raw as u32;
            fill.bytes += 1;
            fill.sprite_table |= in_table;
        }
    }

    /// Note a fill cell writing VSRAM word `index`.
    #[inline(always)]
    pub(super) fn note_fill_vsram_cell(&mut self, index: usize) {
        if self.recording.is_some() {
            if let Some(fill) = self.live_writes().and_then(|live| live.fill.as_mut()) {
                fill.last = index as u32;
                fill.bytes += 2;
            }
        }
    }

    /// Note that the fill running has written its last cell.
    #[inline(always)]
    pub(super) fn note_fill_end(&mut self) {
        if self.recording.is_some() {
            self.record_fill_end();
        }
    }

    #[inline(never)]
    fn record_fill_end(&mut self) {
        let Some(live) = self.live_writes() else {
            return;
        };
        let Some(fill) = live.fill.take() else {
            return;
        };
        live.log(record(
            WriteKind::Fill,
            fill.stamp,
            fill.value,
            fill.target,
            fill.bytes,
            fill.start,
            fill.last,
        ));
        live.raise(VideoEvent::TransferComplete(fill.target));
        if fill.sprite_table {
            live.raise(VideoEvent::SpriteTableTransfer);
        }
    }

    /// Note that a transfer from 68000 memory is about to write `words`
    /// words into VRAM, from the address register's value `first` on,
    /// `step` bytes apart.
    #[inline(always)]
    pub(super) fn note_memory_transfer_start(&mut self, first: u16, step: u16, words: usize) {
        if self.recording.is_some() {
            self.record_memory_transfer_start(first, step, words);
        }
    }

    #[inline(never)]
    fn record_memory_transfer_start(&mut self, first: u16, step: u16, words: usize) {
        self.journal_vram_transfer(first & 0xFFFE, step, words);
        let Some(recording) = self.recording.as_deref_mut() else {
            return;
        };
        let indices = (0..words).flat_map(|word| {
            let address =
                usize::from(first.wrapping_add((word as u16).wrapping_mul(step)) & 0xFFFE);
            [address, address + 1]
        });
        recording.live.keep_before(&self.vram, indices);
    }

    /// Note a transfer from 68000 memory that has just written its data.
    #[inline(always)]
    pub(super) fn note_memory_transfer(&mut self, transfer: MemoryTransfer) {
        if self.recording.is_some() {
            self.record_memory_transfer(transfer);
        }
    }

    #[inline(never)]
    fn record_memory_transfer(&mut self, transfer: MemoryTransfer) {
        let Some(target) = WriteTarget::of_access_code(transfer.code) else {
            return;
        };
        if transfer.words == 0 {
            return;
        }
        if !self.records_live() {
            return;
        }
        let stamp = self.write_stamp();
        let table = self.sprite_table_bytes();
        let Some(recording) = self.recording.as_deref_mut() else {
            return;
        };
        let live = &mut recording.live;
        // Word by word, for the sources and the events; the record alone
        // needs only the transfer's ends.
        let words = if live.memory.is_some() || live.events.is_some() {
            transfer.words
        } else {
            0
        };
        let mut sprite_table = false;
        for word in 0..words {
            let destination = transfer.destination_of_word(word);
            match target {
                WriteTarget::Vram => {
                    let address = usize::from(destination & 0xFFFE);
                    sprite_table |= table.contains(&address);
                    if live.memory.is_some() {
                        // The word's high byte comes from the even address;
                        // an odd destination swaps the two in VRAM.
                        let high = transfer.source_of_word(word);
                        let (first, second) = if destination & 1 == 0 {
                            (high, high | 1)
                        } else {
                            (high | 1, high)
                        };
                        let first = WriteOrigin::transfer(0, first);
                        let second = WriteOrigin::transfer(0, second);
                        live.set_origin(address, first, stamp.frame);
                        live.set_origin(address + 1, second, stamp.frame);
                    }
                }
                WriteTarget::Cram => {
                    live.raise(VideoEvent::PaletteWrite(target.place(destination) as u8));
                }
                WriteTarget::Vsram => {}
            }
        }
        live.raise_rewritten_tiles(&self.vram);
        live.raise(VideoEvent::TransferComplete(target));
        if sprite_table {
            live.raise(VideoEvent::SpriteTableTransfer);
        }
        let last = transfer.destination_of_word(transfer.words - 1);
        live.log(record(
            WriteKind::Transfer,
            stamp,
            transfer.source,
            target,
            (transfer.words * 2) as u32,
            target.place(transfer.destination),
            target.place(last),
        ));
    }

    /// Note that a copy within VRAM is about to write `length` bytes, from
    /// the address register's value `first` on, `step` bytes apart.
    #[inline(always)]
    pub(super) fn note_copy_start(&mut self, first: u16, step: u16, length: usize) {
        if self.recording.is_some() {
            self.record_copy_start(first, step, length);
        }
    }

    #[inline(never)]
    fn record_copy_start(&mut self, first: u16, step: u16, length: usize) {
        // Each destination byte lies in the word at its address with bit 0
        // cleared; the journal takes those words before any is written.
        self.journal_vram_transfer(first & 0xFFFE, step, length);
        let Some(recording) = self.recording.as_deref_mut() else {
            return;
        };
        let indices = (0..length)
            .map(|byte| usize::from(first.wrapping_add((byte as u16).wrapping_mul(step)) ^ 1));
        recording.live.keep_before(&self.vram, indices);
    }

    /// Note a copy within VRAM that has just written its `length` bytes:
    /// read from the source register's value `source` on, one byte apart,
    /// written from `first` on, `step` bytes apart.
    #[inline(always)]
    pub(super) fn note_copy(&mut self, source: u16, first: u16, step: u16, length: usize) {
        if self.recording.is_some() {
            self.record_copy(source, first, step, length);
        }
    }

    #[inline(never)]
    fn record_copy(&mut self, source: u16, first: u16, step: u16, length: usize) {
        if length == 0 || !self.records_live() {
            return;
        }
        let stamp = self.write_stamp();
        let table = self.sprite_table_bytes();
        let Some(recording) = self.recording.as_deref_mut() else {
            return;
        };
        let live = &mut recording.live;
        let destination = |byte: usize| first.wrapping_add((byte as u16).wrapping_mul(step));
        // Byte by byte, for the sources and the events; the record alone
        // needs only the copy's ends.
        let bytes = if live.memory.is_some() || live.events.is_some() {
            length
        } else {
            0
        };
        let mut sprite_table = false;
        for byte in 0..bytes {
            let index = usize::from(destination(byte) ^ 1);
            sprite_table |= table.contains(&index);
            if live.memory.is_some() {
                let read = u32::from(source.wrapping_add(byte as u16) ^ 1);
                live.set_origin(index, WriteOrigin::new(Writer::Copy, 0, read), stamp.frame);
            }
        }
        live.raise_rewritten_tiles(&self.vram);
        live.raise(VideoEvent::TransferComplete(WriteTarget::Vram));
        if sprite_table {
            live.raise(VideoEvent::SpriteTableTransfer);
        }
        live.log(record(
            WriteKind::Copy,
            stamp,
            u32::from(source),
            WriteTarget::Vram,
            length as u32,
            u32::from(first),
            u32::from(destination(length - 1)),
        ));
    }

    /// Note that the debugger wrote `count` bytes of VRAM from `start` on
    /// (wrapping at the end), in the order [`Self::get_vram`] gives VRAM.
    pub(super) fn note_debugger_vram_store(&mut self, start: usize, count: usize) {
        let frame = self.frame_being_drawn;
        let Some(memory) = self.live_writes().and_then(|live| live.memory.as_mut()) else {
            return;
        };
        for offset in 0..count.min(VRAM_BYTES) {
            memory.note((start + offset) % VRAM_BYTES, WriteOrigin::DEBUGGER, frame);
        }
    }
}

/// How a palette colour was written, which says what the live recorders
/// note of it besides the line table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PaletteStore {
    /// The 68000 stored it through the data port: a record of its own, and
    /// the palette event.
    Port,
    /// The 68000's store that starts a fill: the palette event; the fill's
    /// record covers it.
    FillStart,
    /// A fill's cell: the palette event; the fill's record covers it.
    FillCell,
    /// A transfer's colour, which the transfer's record and events cover,
    /// or a colour a transfer metered across the picture shows on its line:
    /// nothing more.
    Transfer,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A VDP in the 40-cell mode, writes on, recording frame writes.
    fn recording_writes() -> Vdp {
        let mut vdp = Vdp::new();
        vdp.set_test_register(1, 0x44);
        vdp.set_test_register(12, 0x81);
        vdp.set_test_register(15, 2);
        vdp.set_capture_recording(CaptureRecording {
            frame_writes: true,
            ..CaptureRecording::default()
        });
        vdp
    }

    #[test]
    fn a_take_holds_at_most_its_cap_and_counts_the_rest() {
        let mut vdp = recording_writes();
        vdp.write_word_port(0xC0_0004, 0xC000);
        vdp.write_word_port(0xC0_0004, 0x0000);
        let stores = MOST_RECORDS + 1_000;
        for store in 0..stores {
            vdp.write_word_port(0xC0_0000, (store as u16) << 1);
        }
        let writes = vdp.take_frame_writes().expect("frame writes armed");
        assert_eq!(writes.records.len(), MOST_RECORDS * WORDS_PER_RECORD);
        assert_eq!(writes.dropped, 1_000);
        let first = &writes.records[..WORDS_PER_RECORD];
        assert_eq!(first[0], WriteKind::PaletteStore as u32 | (3 << 16));
        assert_eq!(first[4], WriteTarget::Cram as u32 | (2 << 8));
        assert_eq!((first[5], first[6]), (0, 0));
        let second = &writes.records[WORDS_PER_RECORD..2 * WORDS_PER_RECORD];
        assert_eq!((second[5], second[6]), (1, 2), "colour 1 became $0002");

        let again = vdp.take_frame_writes().expect("still armed");
        assert!(
            again.records.is_empty() && again.dropped == 0,
            "a take empties it"
        );
    }

    #[test]
    fn an_event_is_raised_once_and_only_when_watched() {
        let mut watch = VideoEventWatch::default();
        assert!(watch.is_empty());
        watch.palette_colours.insert(5);
        watch.tiles.insert(2047);
        watch.tiles.insert(2048);
        assert!(watch.watches(VideoEvent::PaletteWrite(5)));
        assert!(!watch.watches(VideoEvent::PaletteWrite(6)));
        assert!(watch.watches(VideoEvent::TileRewritten(2047)));
        assert!(!watch.watches(VideoEvent::TileRewritten(0)));
        assert!(!watch.watches(VideoEvent::SpriteTableTransfer));
        assert!(!watch.watches(VideoEvent::TransferComplete(WriteTarget::Vram)));
        watch.transfer_targets.insert(WriteTarget::Cram as u32);
        watch.sprite_table = true;
        assert!(watch.watches(VideoEvent::TransferComplete(WriteTarget::Cram)));
        assert!(!watch.watches(VideoEvent::TransferComplete(WriteTarget::Vram)));
        assert!(watch.watches(VideoEvent::SpriteTableTransfer));

        let mut state = EventState {
            watch,
            raised: RaisedEvents::new(MOST_RAISED_EVENTS),
        };
        for _ in 0..3 {
            state.raise(VideoEvent::PaletteWrite(5));
        }
        state.raise(VideoEvent::PaletteWrite(6));
        state.raise(VideoEvent::TileRewritten(2047));
        let mut drained = Vec::new();
        state.raised.drain(|event| drained.push(event));
        assert_eq!(
            drained,
            [VideoEvent::PaletteWrite(5), VideoEvent::TileRewritten(2047)]
        );
    }

    #[test]
    fn a_transfer_word_names_its_source_inside_the_window() {
        let transfer = MemoryTransfer {
            source: 0x01_FFFC,
            destination: 0x0001,
            step: 2,
            words: 4,
            code: 0x01,
        };
        let sources: Vec<u32> = (0..4).map(|word| transfer.source_of_word(word)).collect();
        assert_eq!(sources, [0x01_FFFC, 0x01_FFFE, 0x00_0000, 0x00_0002]);
        assert_eq!(transfer.destination_of_word(3), 0x0007);
    }
}
