//! The sound log: one timeline per console of what its sound chips were
//! told, every entry stamped on one clock.
//!
//! A host starts the log with [`crate::audio::SoundCore::start_sound_log`]
//! and takes what was logged with
//! [`crate::audio::SoundCore::take_sound_log`]. The first entries of the
//! first take are the chips as they stood when the log began (the image),
//! all at time 0: their registers, in a log of the writes, and a key-on for
//! each channel then sounding, in a log of the keys. The times never go
//! back; a frame's end is an entry of its own, in every log. A load, a reset
//! or a power cycle between two takes marks the later one interrupted (every
//! note held before it ends there), and the log goes on with the chips'
//! image again at the time it stood at.
//!
//! The record form ([`SoundLog::records`]) packs each entry in
//! [`RECORD_BYTES`] bytes, little-endian, for a host that moves logs
//! between WebAssembly and its own code.

use serde::{Deserialize, Serialize};

/// What one entry says a chip was told or did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SoundEvent {
    /// A value written to one of the chip's registers: on a chip with two
    /// register banks, the bank in the high byte.
    Write { register: u16, value: u8 },
    /// The chip cleared by a reset line its program drives; the chip's
    /// registers as the reset left them follow at the same time.
    Reset,
    /// The end of one of the machine's frames.
    FrameEnd,
    /// A channel keyed on: a note starts. `instrument` names what it plays
    /// (the sample's number on a chip that plays samples, a hash of the
    /// channel's patch on an FM chip), `pitch` is the chip's own value for
    /// the note, and `level` the chip's own measure of its loudness (on a
    /// chip that plays samples the larger of the voice's two volumes, louder
    /// as it grows; on an FM chip the attenuation of the channel's loudest
    /// carrier, softer as it grows).
    KeyOn {
        channel: u8,
        instrument: u64,
        pitch: u32,
        level: u16,
    },
    /// A channel keyed off, or cut by its chip: its note ends.
    KeyOff { channel: u8 },
    /// A channel's sample reached an end with no loop, and the voice fell
    /// silent there.
    SampleEnd { channel: u8 },
}

/// One entry of a [`SoundLog`]: when, which chip, what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SoundLogEntry {
    /// In the log's clock ([`SoundLog::clock_hz`]), from when it began.
    pub time: u64,
    /// The chip, the index of [`crate::audio::SoundCore::sound_log_chips`]
    /// (for a frame's end, the console's first chip).
    pub chip: u8,
    pub event: SoundEvent,
}

/// A sound chip a log names, with what a file of it needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SoundChipKind {
    /// The Yamaha YM2612, on its input clock.
    Ym2612 { clock_hz: u32 },
    /// The Texas Instruments SN76489, on its input clock.
    Sn76489 { clock_hz: u32 },
    /// The SNES's S-DSP.
    SDsp,
}

/// What a log holds; every log holds the frames' ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundLogContents {
    /// The chips' register writes, and their resets.
    pub writes: bool,
    /// Each channel's key-on and key-off, and a sample's end, as the chips
    /// work them out of their writes: an entry where a channel's keyed
    /// state changes, none for a write that leaves it as it was.
    pub keys: bool,
}

impl SoundLogContents {
    /// Whether it holds anything.
    pub fn any(&self) -> bool {
        self.writes || self.keys
    }
}

/// A take of the sound log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SoundLog {
    /// What an entry's time counts, per second: the Mega Drive's master
    /// clocks, the S-DSP's samples.
    pub clock_hz: u64,
    pub entries: Vec<SoundLogEntry>,
    /// How many of the first entries are the chips' registers as they
    /// stood when the log began: none after the first take.
    pub image_len: usize,
    /// Whether a load, a reset or a power cycle came since the take before.
    pub interrupted: bool,
}

/// The bytes an entry takes in the record form.
pub const RECORD_BYTES: usize = 24;

/// The kinds of the record form, in the low half of its byte 8.
const KIND_WRITE: u8 = 0;
const KIND_RESET: u8 = 1;
const KIND_FRAME_END: u8 = 2;
const KIND_KEY_ON: u8 = 3;
const KIND_KEY_OFF: u8 = 4;
const KIND_SAMPLE_END: u8 = 5;

impl SoundLog {
    /// The entries in the record form, [`RECORD_BYTES`] each: bytes 0-7 the
    /// time; byte 8 the kind in its low four bits (0 a write, 1 a reset, 2
    /// a frame's end, 3 a key-on, 4 a key-off, 5 a sample's end) and the
    /// chip in its high four. A write: byte 9 the value, bytes 10-11 the
    /// register. A key-on: byte 9 the channel, bytes 10-11 the level, 12-15
    /// the pitch, 16-23 the instrument. A key-off and a sample's end: byte 9
    /// the channel. The bytes a kind does not use are zero.
    pub fn records(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.entries.len() * RECORD_BYTES);
        for entry in &self.entries {
            let mut record = [0u8; RECORD_BYTES];
            record[..8].copy_from_slice(&entry.time.to_le_bytes());
            let kind = match entry.event {
                SoundEvent::Write { register, value } => {
                    record[9] = value;
                    record[10..12].copy_from_slice(&register.to_le_bytes());
                    KIND_WRITE
                }
                SoundEvent::Reset => KIND_RESET,
                SoundEvent::FrameEnd => KIND_FRAME_END,
                SoundEvent::KeyOn {
                    channel,
                    instrument,
                    pitch,
                    level,
                } => {
                    record[9] = channel;
                    record[10..12].copy_from_slice(&level.to_le_bytes());
                    record[12..16].copy_from_slice(&pitch.to_le_bytes());
                    record[16..24].copy_from_slice(&instrument.to_le_bytes());
                    KIND_KEY_ON
                }
                SoundEvent::KeyOff { channel } => {
                    record[9] = channel;
                    KIND_KEY_OFF
                }
                SoundEvent::SampleEnd { channel } => {
                    record[9] = channel;
                    KIND_SAMPLE_END
                }
            };
            record[8] = kind | entry.chip << 4;
            out.extend_from_slice(&record);
        }
        out
    }

    /// The entries the record form `records` holds.
    pub fn entries_of(records: &[u8]) -> Result<Vec<SoundLogEntry>, String> {
        if !records.len().is_multiple_of(RECORD_BYTES) {
            return Err(format!(
                "{} bytes are not whole records of {RECORD_BYTES}",
                records.len()
            ));
        }
        records
            .as_chunks::<RECORD_BYTES>()
            .0
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let time = u64::from_le_bytes(record[..8].try_into().expect("8 bytes"));
                let event = match record[8] & 0x0F {
                    KIND_WRITE => SoundEvent::Write {
                        register: u16::from_le_bytes([record[10], record[11]]),
                        value: record[9],
                    },
                    KIND_RESET => SoundEvent::Reset,
                    KIND_FRAME_END => SoundEvent::FrameEnd,
                    KIND_KEY_ON => SoundEvent::KeyOn {
                        channel: record[9],
                        level: u16::from_le_bytes([record[10], record[11]]),
                        pitch: u32::from_le_bytes(record[12..16].try_into().expect("4 bytes")),
                        instrument: u64::from_le_bytes(record[16..24].try_into().expect("8 bytes")),
                    },
                    KIND_KEY_OFF => SoundEvent::KeyOff { channel: record[9] },
                    KIND_SAMPLE_END => SoundEvent::SampleEnd { channel: record[9] },
                    kind => return Err(format!("record {index}: no kind {kind}")),
                };
                Ok(SoundLogEntry {
                    time,
                    chip: record[8] >> 4,
                    event,
                })
            })
            .collect()
    }
}

/// Two chips' entries, each in time order, as one timeline: by time, and
/// `first`'s before `second`'s at the same time.
pub fn merge_by_time(first: &[SoundLogEntry], second: &[SoundLogEntry]) -> Vec<SoundLogEntry> {
    let mut merged = Vec::with_capacity(first.len() + second.len());
    let (mut a, mut b) = (first.iter().peekable(), second.iter().peekable());
    while let (Some(x), Some(y)) = (a.peek(), b.peek()) {
        if y.time < x.time {
            merged.push(**y);
            b.next();
        } else {
            merged.push(**x);
            a.next();
        }
    }
    merged.extend(a.copied());
    merged.extend(b.copied());
    merged
}

/// The log a core keeps while a host logs: what was logged since the last
/// take, the image at its head on the first, and whether an interruption
/// came between.
#[derive(Debug, Clone, Default)]
pub struct SoundLogger {
    contents: SoundLogContents,
    entries: Vec<SoundLogEntry>,
    image_len: usize,
    interrupted: bool,
    /// The latest time logged, which an interruption carries on from.
    latest: u64,
}

impl SoundLogger {
    /// Whether a log is running, and what it holds.
    pub fn contents(&self) -> SoundLogContents {
        self.contents
    }

    /// Whether a log is running.
    pub fn is_on(&self) -> bool {
        self.contents.any()
    }

    /// Whether the log takes the chips' writes down.
    pub fn logs_writes(&self) -> bool {
        self.contents.writes
    }

    /// Whether the log takes the channels' keys down.
    pub fn logs_keys(&self) -> bool {
        self.contents.keys
    }

    /// Start a log holding `contents`, its first entries `image`, at time
    /// 0; what an earlier log left is dropped.
    pub fn start(&mut self, contents: SoundLogContents, image: Vec<SoundLogEntry>) {
        *self = Self {
            contents,
            image_len: image.len(),
            entries: image,
            ..Self::default()
        };
    }

    /// Stop logging; what was logged waits for the next take.
    pub fn stop(&mut self) {
        self.contents = SoundLogContents::default();
    }

    /// Log `entries`, in time order and none before the latest.
    pub fn extend(&mut self, entries: impl IntoIterator<Item = SoundLogEntry>) {
        for entry in entries {
            debug_assert!(entry.time >= self.latest, "a sound log's time went back");
            self.latest = self.latest.max(entry.time);
            self.entries.push(entry);
        }
    }

    /// The latest time logged.
    pub fn latest(&self) -> u64 {
        self.latest
    }

    /// A load, a reset or a power cycle came: the next take says so.
    pub fn interrupt(&mut self) {
        self.interrupted = true;
    }

    /// What was logged since the last take; `None` while no log runs and
    /// nothing waits.
    pub fn take(&mut self, clock_hz: u64) -> Option<SoundLog> {
        if !self.is_on() && self.entries.is_empty() && !self.interrupted {
            return None;
        }
        Some(SoundLog {
            clock_hz,
            entries: std::mem::take(&mut self.entries),
            image_len: std::mem::take(&mut self.image_len),
            interrupted: std::mem::take(&mut self.interrupted),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(time: u64, chip: u8, register: u16, value: u8) -> SoundLogEntry {
        SoundLogEntry {
            time,
            chip,
            event: SoundEvent::Write { register, value },
        }
    }

    #[test]
    fn the_record_form_round_trips_and_lays_out_its_bytes() {
        let log = SoundLog {
            clock_hz: 1000,
            entries: vec![
                write(0x0102_0304_0506_0708, 1, 0x1A2B, 0xCD),
                SoundLogEntry {
                    time: 9,
                    chip: 0,
                    event: SoundEvent::Reset,
                },
                SoundLogEntry {
                    time: 10,
                    chip: 0,
                    event: SoundEvent::FrameEnd,
                },
                SoundLogEntry {
                    time: 11,
                    chip: 1,
                    event: SoundEvent::KeyOn {
                        channel: 5,
                        instrument: 0x1122_3344_5566_7788,
                        pitch: 0x0A0B_0C0D,
                        level: 0x0E0F,
                    },
                },
                SoundLogEntry {
                    time: 12,
                    chip: 0,
                    event: SoundEvent::KeyOff { channel: 7 },
                },
                SoundLogEntry {
                    time: 13,
                    chip: 0,
                    event: SoundEvent::SampleEnd { channel: 2 },
                },
            ],
            image_len: 0,
            interrupted: false,
        };
        let records = log.records();
        assert_eq!(records.len(), 6 * RECORD_BYTES);
        assert_eq!(
            &records[..RECORD_BYTES],
            &[8, 7, 6, 5, 4, 3, 2, 1, 0x10, 0xCD, 0x2B, 0x1A, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(records[RECORD_BYTES + 8], 1);
        assert_eq!(records[2 * RECORD_BYTES + 8], 2);
        assert_eq!(
            &records[3 * RECORD_BYTES..4 * RECORD_BYTES],
            &[
                11, 0, 0, 0, 0, 0, 0, 0, 0x13, 5, 0x0F, 0x0E, 0x0D, 0x0C, 0x0B, 0x0A, 0x88, 0x77,
                0x66, 0x55, 0x44, 0x33, 0x22, 0x11
            ]
        );
        assert_eq!(
            &records[4 * RECORD_BYTES + 8..4 * RECORD_BYTES + 10],
            &[4, 7]
        );
        assert_eq!(
            &records[5 * RECORD_BYTES + 8..5 * RECORD_BYTES + 10],
            &[5, 2]
        );
        assert_eq!(SoundLog::entries_of(&records).unwrap(), log.entries);
        assert!(SoundLog::entries_of(&records[1..]).is_err());
        let mut bad = records.clone();
        bad[8] = 9;
        assert!(SoundLog::entries_of(&bad).is_err());
    }

    #[test]
    fn two_chips_merge_by_time_the_first_first_at_a_tie() {
        let a = [write(1, 0, 0, 1), write(5, 0, 0, 2)];
        let b = [write(1, 1, 0, 3), write(3, 1, 0, 4), write(9, 1, 0, 5)];
        let values: Vec<u8> = merge_by_time(&a, &b)
            .iter()
            .map(|entry| match entry.event {
                SoundEvent::Write { value, .. } => value,
                _ => 0,
            })
            .collect();
        assert_eq!(values, [1, 3, 4, 2, 5]);
    }

    #[test]
    fn a_logger_takes_its_image_once_and_says_when_it_was_interrupted() {
        let mut logger = SoundLogger::default();
        assert_eq!(logger.take(1000), None);
        let contents = SoundLogContents {
            writes: true,
            keys: false,
        };
        logger.start(contents, vec![write(0, 0, 0x22, 0)]);
        logger.extend([write(4, 0, 0x28, 0xF0)]);
        let take = logger.take(1000).unwrap();
        assert_eq!((take.entries.len(), take.image_len), (2, 1));
        assert!(!take.interrupted);
        logger.interrupt();
        logger.extend([write(7, 0, 0x28, 0)]);
        let take = logger.take(1000).unwrap();
        assert_eq!((take.entries.len(), take.image_len), (1, 0));
        assert!(take.interrupted);
        assert_eq!(logger.latest(), 7);
        logger.stop();
        assert!(!logger.is_on());
        assert_eq!(logger.take(1000), None);
    }
}
