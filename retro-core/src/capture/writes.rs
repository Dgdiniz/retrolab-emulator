//! Who wrote video memory, and what each frame wrote: the two recorders a
//! core keeps live, as the writes land, while a host asks for them.
//!
//! - [`VideoMemorySources`] keeps, for each byte of video memory, what wrote
//!   it last ([`WriteOrigin`]) and in which frame.
//! - [`WriteLog`] keeps the frame's transfers and palette and sprite-table
//!   writes, eight words a record ([`FrameWrite`]), until a host takes them
//!   ([`FrameWrites`]).
//!
//! The codes are one registry for every core ([`Writer`], [`WriteKind`]); a
//! core uses the ones its hardware has. What each word of a record holds in
//! the core's own units (a dot or a CPU cycle, a port or a memory code) the
//! core documents.

/// What wrote a byte of video memory: bits 28-30 of its [`WriteOrigin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Writer {
    /// Not written since the recorder was turned on or last emptied.
    None = 0,
    /// A transfer from memory (a DMA): the source is where the byte was read.
    Transfer = 1,
    /// A transfer made line by line as the picture is drawn (the SNES HDMA):
    /// the source is where the byte was read.
    LineTransfer = 2,
    /// A store by the processor through the video chip's port.
    Processor = 3,
    /// The debugger.
    Debugger = 4,
    /// A fill: the source is the value the fill wrote.
    Fill = 5,
    /// A copy within video memory: the source is the byte copied from.
    Copy = 6,
}

impl Writer {
    /// The writer `code` names (an origin's [`WriteOrigin::writer`]), or
    /// `None` for a code no writer has.
    pub const fn from_code(code: u32) -> Option<Writer> {
        match code {
            0 => Some(Self::None),
            1 => Some(Self::Transfer),
            2 => Some(Self::LineTransfer),
            3 => Some(Self::Processor),
            4 => Some(Self::Debugger),
            5 => Some(Self::Fill),
            6 => Some(Self::Copy),
            _ => None,
        }
    }
}

/// Who wrote a byte of video memory, packed as a host reads it:
///
/// | Bits | Meaning |
/// |---|---|
/// | 0-23 | where the byte came from: what it means is the writer's ([`Writer`]) and the core's |
/// | 24-26 | the transfer channel, for a writer that has channels; 0 otherwise |
/// | 28-30 | the writer ([`Writer`]) |
///
/// Bits 27 and 31 are 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct WriteOrigin(pub u32);

impl WriteOrigin {
    /// The bits holding where the byte came from, 0-23.
    pub const SOURCE_MASK: u32 = 0x00FF_FFFF;
    /// Where the three bits of the channel start (bits 24-26).
    pub const CHANNEL_SHIFT: u32 = 24;
    /// Where the three bits of the writer start (bits 28-30).
    pub const WRITER_SHIFT: u32 = 28;

    /// Not written since the recorder was turned on or last emptied.
    pub const NONE: Self = Self(0);
    /// The debugger: its source is 0.
    pub const DEBUGGER: Self = Self::new(Writer::Debugger, 0, 0);

    /// A byte `writer` wrote on `channel` (its low 3 bits) from `source`
    /// (its low 24 bits).
    #[inline]
    pub const fn new(writer: Writer, channel: u32, source: u32) -> Self {
        Self(
            ((writer as u32) << Self::WRITER_SHIFT)
                | ((channel & 7) << Self::CHANNEL_SHIFT)
                | (source & Self::SOURCE_MASK),
        )
    }

    /// A byte a transfer on `channel` read from `source`.
    #[inline]
    pub const fn transfer(channel: u32, source: u32) -> Self {
        Self::new(Writer::Transfer, channel, source)
    }

    /// A byte a line-by-line transfer on `channel` read from `source`.
    #[inline]
    pub const fn line_transfer(channel: u32, source: u32) -> Self {
        Self::new(Writer::LineTransfer, channel, source)
    }

    /// A byte the processor stored, `source` naming where its value came
    /// from as the core documents (an instruction's address, or the address
    /// of the last word the processor read).
    #[inline]
    pub const fn processor(source: u32) -> Self {
        Self::new(Writer::Processor, 0, source)
    }

    /// The writer's code, bits 28-30 ([`Writer::from_code`]).
    #[inline]
    pub const fn writer(self) -> u32 {
        (self.0 >> Self::WRITER_SHIFT) & 7
    }

    /// The channel, bits 24-26.
    #[inline]
    pub const fn channel(self) -> u32 {
        (self.0 >> Self::CHANNEL_SHIFT) & 7
    }

    /// Where the byte came from, bits 0-23.
    #[inline]
    pub const fn source(self) -> u32 {
        self.0 & Self::SOURCE_MASK
    }
}

/// Who wrote each byte of a video memory, and in which frame, since the
/// recorder was turned on or last emptied: one packed [`WriteOrigin`] and one
/// frame number a byte, in the byte order the core gives its video memory.
/// A byte not written since holds 0 in both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoMemorySources {
    origins: Box<[u32]>,
    frames: Box<[u32]>,
}

impl VideoMemorySources {
    /// Nothing written yet, for a video memory of `bytes` bytes.
    ///
    /// # Panics
    ///
    /// When `bytes` is not a power of two (every video memory is: 16 KiB on
    /// the NES, 64 KiB on the Mega Drive and the SNES), since a byte index is
    /// taken modulo it.
    pub fn new(bytes: usize) -> Self {
        assert!(
            bytes.is_power_of_two(),
            "video memory of {bytes} bytes: the size must be a power of two"
        );
        Self {
            origins: vec![0; bytes].into_boxed_slice(),
            frames: vec![0; bytes].into_boxed_slice(),
        }
    }

    /// The bytes of the video memory.
    pub fn len(&self) -> usize {
        self.origins.len()
    }

    /// Whether the video memory has no byte, which [`Self::new`] never makes.
    pub fn is_empty(&self) -> bool {
        self.origins.is_empty()
    }

    /// The packed origin of every byte.
    pub fn origins(&self) -> &[u32] {
        &self.origins
    }

    /// The frame every byte was last written in, its low 32 bits; 0 for a
    /// byte not written. The frame is numbered as the core's records number
    /// it.
    pub fn frames(&self) -> &[u32] {
        &self.frames
    }

    /// The origin of byte `byte`, taken modulo the size.
    #[inline]
    pub fn origin(&self, byte: usize) -> WriteOrigin {
        WriteOrigin(self.origins[byte & (self.len() - 1)])
    }

    /// Byte `byte` (taken modulo the size) was written by `origin` in frame
    /// `frame`, of which its low 32 bits are kept.
    #[inline]
    pub fn note(&mut self, byte: usize, origin: WriteOrigin, frame: u64) {
        let byte = byte & (self.origins.len() - 1);
        self.origins[byte] = origin.0;
        self.frames[byte] = frame as u32;
    }

    /// Forget every byte's writer and frame.
    pub fn clear(&mut self) {
        self.origins.fill(0);
        self.frames.fill(0);
    }
}

/// What a record of the frame-write log is: [`FrameWrite::kind`], bits 0-7
/// of its word 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WriteKind {
    /// A transfer from memory (a DMA): one record a transfer, not one per
    /// byte, colour or entry it wrote.
    Transfer = 1,
    /// One unit of a line-by-line transfer: a channel's transfer for a line
    /// (the SNES HDMA).
    LineTransferUnit = 2,
    /// A line-by-line transfer's channel set up as the frame starts them.
    LineTransferSetUp = 3,
    /// A palette colour the processor or a line-by-line transfer stored
    /// through the port.
    PaletteStore = 4,
    /// A store into the sprite table by the processor or a line-by-line
    /// transfer, through the port.
    SpriteTableStore = 5,
    /// A fill.
    Fill = 6,
    /// A copy within video memory.
    Copy = 7,
}

/// The words of one record of the frame-write log.
pub const WORDS_PER_RECORD: usize = 8;

/// The most a record's count holds: the 24 bits of word 4 above its target.
const MOST_COUNTED: u32 = 0x00FF_FFFF;

/// One record of the frame-write log, unpacked: what [`Self::pack`] puts into
/// its eight words.
///
/// | Word | Contents |
/// |---|---|
/// | 0 | bits 0-7 `kind`; 8-10 `channel`; 11 `port_to_memory`; 12-14 `unit`; 15 `indirect`; 16-18 `writer`; 20 `nothing_landed` |
/// | 1 | `frame`, its low 32 bits |
/// | 2 | bits 0-15 `line`; 16-31 `position` |
/// | 3 | `source` |
/// | 4 | bits 0-7 `target`; 8-31 `count`, at most 0xFF_FFFF |
/// | 5 | `start` |
/// | 6 | `end_or_value` |
/// | 7 | `extra` |
///
/// What each field holds for each kind, in the core's units, the core
/// documents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameWrite {
    /// What the record is ([`WriteKind`]).
    pub kind: u8,
    /// The transfer channel (0-7), for a writer that has channels.
    pub channel: u8,
    /// The transfer reads from the ports and writes to memory, the reverse
    /// of a transfer into video memory (the SNES's B bus to A bus).
    pub port_to_memory: bool,
    /// The transfer's unit (0-7): how its bytes go to the ports.
    pub unit: u8,
    /// A line-by-line transfer's channel reads its data through a pointer.
    pub indirect: bool,
    /// For a store (kinds 4 and 5), the writer ([`Writer`]).
    pub writer: u8,
    /// A transfer none of whose bytes reached the memory its port writes.
    pub nothing_landed: bool,
    /// The frame the write was made in.
    pub frame: u64,
    /// The line the write was made on.
    pub line: u16,
    /// Where on the line, in the core's unit (a dot, or the processor's
    /// cycles into the line).
    pub position: u16,
    /// Where the first byte came from, or a value standing for it.
    pub source: u32,
    /// The memory or port written, as the core codes it.
    pub target: u8,
    /// The bytes moved or written; packed saturating at 0xFF_FFFF.
    pub count: u32,
    /// Where the writes started.
    pub start: u32,
    /// Where they ended, or the value written.
    pub end_or_value: u32,
    /// A word of the core's own; 0 where it has none.
    pub extra: u32,
}

impl FrameWrite {
    /// The record's eight words.
    #[inline]
    pub fn pack(&self) -> [u32; WORDS_PER_RECORD] {
        let head = u32::from(self.kind)
            | (u32::from(self.channel & 7) << 8)
            | (u32::from(self.port_to_memory) << 11)
            | (u32::from(self.unit & 7) << 12)
            | (u32::from(self.indirect) << 15)
            | (u32::from(self.writer & 7) << 16)
            | (u32::from(self.nothing_landed) << 20);
        [
            head,
            self.frame as u32,
            u32::from(self.line) | (u32::from(self.position) << 16),
            self.source,
            u32::from(self.target) | (self.count.min(MOST_COUNTED) << 8),
            self.start,
            self.end_or_value,
            self.extra,
        ]
    }

    /// A record read back from its eight words. The frame's high bits are
    /// not in the record, and a count packed past 0xFF_FFFF reads as
    /// 0xFF_FFFF.
    pub fn unpack(words: &[u32; WORDS_PER_RECORD]) -> Self {
        let head = words[0];
        Self {
            kind: head as u8,
            channel: ((head >> 8) & 7) as u8,
            port_to_memory: head & (1 << 11) != 0,
            unit: ((head >> 12) & 7) as u8,
            indirect: head & (1 << 15) != 0,
            writer: ((head >> 16) & 7) as u8,
            nothing_landed: head & (1 << 20) != 0,
            frame: u64::from(words[1]),
            line: words[2] as u16,
            position: (words[2] >> 16) as u16,
            source: words[3],
            target: words[4] as u8,
            count: words[4] >> 8,
            start: words[5],
            end_or_value: words[6],
            extra: words[7],
        }
    }
}

/// The records taken out of a [`WriteLog`]: what a host reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameWrites {
    /// The records, [`WORDS_PER_RECORD`] words each, oldest first.
    pub records: Vec<u32>,
    /// The records that did not fit in the log since the last take.
    pub dropped: u32,
}

impl FrameWrites {
    /// The records, unpacked.
    pub fn iter(&self) -> impl Iterator<Item = FrameWrite> + '_ {
        self.records
            .as_chunks::<WORDS_PER_RECORD>()
            .0
            .iter()
            .map(FrameWrite::unpack)
    }
}

/// The records of the frame's writes between two takes, at most a number of
/// records given at construction; past it a record is counted in
/// [`FrameWrites::dropped`] instead.
#[derive(Debug)]
pub struct WriteLog {
    words: Vec<u32>,
    dropped: u32,
    capacity_records: usize,
}

impl WriteLog {
    /// An empty log keeping at most `capacity_records` records a take.
    pub fn new(capacity_records: usize) -> Self {
        Self {
            words: Vec::new(),
            dropped: 0,
            capacity_records,
        }
    }

    /// The records the log holds now.
    #[inline]
    pub fn len(&self) -> usize {
        self.words.len() / WORDS_PER_RECORD
    }

    /// Whether the log holds no record.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Add `record`, or count it dropped when the log is full.
    #[inline]
    pub fn push(&mut self, record: &[u32; WORDS_PER_RECORD]) {
        if self.len() >= self.capacity_records {
            self.dropped = self.dropped.saturating_add(1);
        } else {
            self.words.extend_from_slice(record);
        }
    }

    /// Keep room for a record filled later ([`Self::fill`]), so that it
    /// comes before the records pushed meanwhile, and return its index; or
    /// count it dropped when the log is full. The room holds zeroes until it
    /// is filled.
    #[inline]
    pub fn reserve(&mut self) -> Option<usize> {
        let index = self.len();
        if index >= self.capacity_records {
            self.dropped = self.dropped.saturating_add(1);
            return None;
        }
        self.words.extend([0; WORDS_PER_RECORD]);
        Some(index)
    }

    /// Put `record` in the room [`Self::reserve`] kept at `index`.
    ///
    /// # Panics
    ///
    /// When `index` names no record of the log: room is kept only until the
    /// next take or clear.
    #[inline]
    pub fn fill(&mut self, index: usize, record: &[u32; WORDS_PER_RECORD]) {
        let at = index * WORDS_PER_RECORD;
        self.words[at..at + WORDS_PER_RECORD].copy_from_slice(record);
    }

    /// Everything since the last take, the log then empty, with the same
    /// capacity.
    pub fn take(&mut self) -> FrameWrites {
        let capacity = self.words.capacity();
        FrameWrites {
            records: std::mem::replace(&mut self.words, Vec::with_capacity(capacity)),
            dropped: std::mem::take(&mut self.dropped),
        }
    }

    /// Forget every record and the count of those dropped.
    pub fn clear(&mut self) {
        self.words.clear();
        self.dropped = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_writer_is_read_back_from_its_code() {
        let writers = [
            Writer::None,
            Writer::Transfer,
            Writer::LineTransfer,
            Writer::Processor,
            Writer::Debugger,
            Writer::Fill,
            Writer::Copy,
        ];
        for (code, writer) in writers.into_iter().enumerate() {
            assert_eq!(writer as u32, code as u32);
            assert_eq!(Writer::from_code(code as u32), Some(writer));
        }
        assert_eq!(Writer::from_code(7), None);
        assert_eq!(Writer::from_code(u32::MAX), None);
    }

    #[test]
    fn an_origin_packs_its_writer_channel_and_source() {
        let transfer = WriteOrigin::transfer(5, 0x7E_1234);
        assert_eq!(transfer.0, 0x157E_1234);
        assert_eq!(
            (transfer.writer(), transfer.channel(), transfer.source()),
            (1, 5, 0x7E_1234)
        );
        let line = WriteOrigin::line_transfer(7, 0x1FF_FFFF);
        assert_eq!(line.0, 0x27FF_FFFF, "the source keeps its 24 bits");
        assert_eq!(
            (line.writer(), line.channel(), line.source()),
            (2, 7, 0xFF_FFFF)
        );
        let store = WriteOrigin::processor(0x80_8123);
        assert_eq!(store.0, 0x3080_8123);
        assert_eq!(store.channel(), 0);
        assert_eq!(WriteOrigin::DEBUGGER.0, 0x4000_0000);
        assert_eq!(WriteOrigin::NONE.writer(), Writer::None as u32);
        let fill = WriteOrigin::new(Writer::Fill, 0, 0xAB);
        assert_eq!(fill.0, 0x5000_00AB);
        assert_eq!(WriteOrigin::new(Writer::Copy, 9, 0xFFFF).0, 0x6100_FFFF);
        assert_eq!(
            WriteOrigin::SOURCE_MASK
                | (7 << WriteOrigin::CHANNEL_SHIFT)
                | (7 << WriteOrigin::WRITER_SHIFT),
            0x77FF_FFFF,
            "bits 27 and 31 are never set"
        );
        assert_eq!(Writer::from_code(fill.writer()), Some(Writer::Fill));
    }

    #[test]
    fn the_sources_note_each_byte_its_origin_and_frame() {
        let mut sources = VideoMemorySources::new(0x4000);
        assert_eq!((sources.len(), sources.is_empty()), (0x4000, false));
        assert!(sources.origins().iter().all(|&origin| origin == 0));
        assert!(sources.frames().iter().all(|&frame| frame == 0));

        sources.note(3, WriteOrigin::processor(0x1234), 0x1_0000_0002);
        assert_eq!(sources.origin(3), WriteOrigin::processor(0x1234));
        assert_eq!(sources.frames()[3], 2, "the frame keeps its low 32 bits");
        assert_eq!(sources.origins()[3], 0x3000_1234);

        // A byte past the end is taken modulo the size.
        sources.note(0x4000 + 5, WriteOrigin::DEBUGGER, 9);
        assert_eq!(sources.origin(5), WriteOrigin::DEBUGGER);
        assert_eq!(sources.origin(0x4000 + 5), WriteOrigin::DEBUGGER);
        assert_eq!(sources.frames()[5], 9);

        sources.clear();
        assert_eq!(sources.origin(3), WriteOrigin::NONE);
        assert!(sources.frames().iter().all(|&frame| frame == 0));
        assert_eq!(sources.len(), 0x4000, "a clear keeps the size");
    }

    #[test]
    #[should_panic(expected = "must be a power of two")]
    fn a_video_memory_is_a_power_of_two_bytes() {
        VideoMemorySources::new(3000);
    }

    #[test]
    fn the_kinds_keep_their_codes() {
        let kinds = [
            WriteKind::Transfer,
            WriteKind::LineTransferUnit,
            WriteKind::LineTransferSetUp,
            WriteKind::PaletteStore,
            WriteKind::SpriteTableStore,
            WriteKind::Fill,
            WriteKind::Copy,
        ];
        assert_eq!(kinds.map(|kind| kind as u8), [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(WORDS_PER_RECORD, 8);
    }

    #[test]
    fn a_record_packs_and_unpacks_every_field() {
        let record = FrameWrite {
            kind: WriteKind::LineTransferUnit as u8,
            channel: 6,
            port_to_memory: true,
            unit: 3,
            indirect: true,
            writer: Writer::LineTransfer as u8,
            nothing_landed: true,
            frame: 0x1_2345_6789,
            line: 224,
            position: 276,
            source: 0x7E_8000,
            target: 0x22,
            count: 4,
            start: 0x30,
            end_or_value: 0x1234_5678,
            extra: 0x7F_0000,
        };
        let words = record.pack();
        assert_eq!(
            words[0],
            2 | (6 << 8) | (1 << 11) | (3 << 12) | (1 << 15) | (2 << 16) | (1 << 20)
        );
        assert_eq!(words[1], 0x2345_6789, "the frame's low 32 bits");
        assert_eq!(words[2], 224 | (276 << 16));
        assert_eq!(words[3], 0x7E_8000);
        assert_eq!(words[4], 0x22 | (4 << 8));
        assert_eq!(
            (words[5], words[6], words[7]),
            (0x30, 0x1234_5678, 0x7F_0000)
        );
        assert_eq!(
            FrameWrite::unpack(&words),
            FrameWrite {
                frame: 0x2345_6789,
                ..record
            }
        );
    }

    #[test]
    fn a_store_by_the_processor_names_its_writer_in_word_zero() {
        let store = FrameWrite {
            kind: WriteKind::PaletteStore as u8,
            writer: Writer::Processor as u8,
            frame: 7,
            line: 3,
            position: 0xFFFF,
            source: 0x00FF_0010,
            target: 1,
            count: 2,
            start: 5,
            end_or_value: 0x0EEE,
            ..FrameWrite::default()
        };
        assert_eq!(
            store.pack(),
            [
                4 | (3 << 16),
                7,
                3 | (0xFFFF << 16),
                0x00FF_0010,
                1 | (2 << 8),
                5,
                0x0EEE,
                0
            ]
        );
    }

    #[test]
    fn a_count_too_large_for_its_24_bits_is_packed_saturating() {
        let mut record = FrameWrite {
            kind: WriteKind::Fill as u8,
            target: 2,
            count: 0x00FF_FFFF,
            ..FrameWrite::default()
        };
        assert_eq!(record.pack()[4], 2 | (0x00FF_FFFF << 8));
        record.count = 0x0100_0000;
        assert_eq!(record.pack()[4], 2 | (0x00FF_FFFF << 8));
        record.count = u32::MAX;
        assert_eq!(FrameWrite::unpack(&record.pack()).count, 0x00FF_FFFF);
        assert_eq!(FrameWrite::unpack(&record.pack()).target, 2);
    }

    fn record_of_kind(kind: WriteKind, start: u32) -> [u32; WORDS_PER_RECORD] {
        FrameWrite {
            kind: kind as u8,
            start,
            ..FrameWrite::default()
        }
        .pack()
    }

    #[test]
    fn the_log_keeps_its_cap_counts_the_rest_and_empties_on_a_take() {
        let mut log = WriteLog::new(3);
        assert!(log.is_empty());
        for start in 0..5 {
            log.push(&record_of_kind(WriteKind::PaletteStore, start));
        }
        assert_eq!(log.len(), 3);
        let taken = log.take();
        assert_eq!(taken.records.len(), 3 * WORDS_PER_RECORD);
        assert_eq!(taken.dropped, 2);
        let starts: Vec<u32> = taken.iter().map(|record| record.start).collect();
        assert_eq!(starts, [0, 1, 2], "the first records are kept, in order");

        let again = log.take();
        assert!(again.records.is_empty());
        assert_eq!(again.dropped, 0, "a take empties the count too");
        assert!(log.is_empty());

        // The cap holds after a take.
        for start in 0..4 {
            log.push(&record_of_kind(WriteKind::Fill, start));
        }
        assert_eq!((log.len(), log.take().dropped), (3, 1));
    }

    #[test]
    fn a_reserved_record_keeps_its_place_before_the_ones_pushed_after() {
        let mut log = WriteLog::new(8);
        let at = log.reserve().expect("room");
        assert_eq!(at, 0);
        log.push(&record_of_kind(WriteKind::PaletteStore, 1));
        let second = log.reserve().expect("room");
        assert_eq!(second, 2);
        assert_eq!(log.len(), 3);
        log.fill(at, &record_of_kind(WriteKind::LineTransferUnit, 0));
        let kinds: Vec<u8> = log.take().iter().map(|record| record.kind).collect();
        assert_eq!(
            kinds,
            [
                WriteKind::LineTransferUnit as u8,
                WriteKind::PaletteStore as u8,
                0
            ],
            "room not filled holds zeroes"
        );
    }

    #[test]
    fn a_full_log_keeps_no_room_and_counts_it_dropped() {
        let mut log = WriteLog::new(1);
        assert_eq!(log.reserve(), Some(0));
        assert_eq!(log.reserve(), None);
        log.push(&record_of_kind(WriteKind::Copy, 0));
        let taken = log.take();
        assert_eq!(taken.records.len(), WORDS_PER_RECORD);
        assert_eq!(taken.dropped, 2);
    }

    #[test]
    fn a_clear_forgets_the_records_and_the_dropped_count() {
        let mut log = WriteLog::new(1);
        log.push(&record_of_kind(WriteKind::Transfer, 0));
        log.push(&record_of_kind(WriteKind::Transfer, 1));
        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.take(), FrameWrites::default());
    }

    #[test]
    fn the_dropped_count_saturates() {
        let mut log = WriteLog::new(0);
        log.dropped = u32::MAX - 1;
        log.push(&record_of_kind(WriteKind::Transfer, 0));
        assert_eq!(log.reserve(), None);
        log.push(&record_of_kind(WriteKind::Transfer, 0));
        assert_eq!(log.take().dropped, u32::MAX);
    }
}
