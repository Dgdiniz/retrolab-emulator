//! The VGM writer: a [`SoundLog`] as a VGM 1.71 file, for the chips VGM
//! has commands for.
//!
//! The file is the 256-byte header, the commands, and a GD3 tag. Each
//! write becomes its chip's command at its time; the time between two
//! entries becomes waits, worked out from each entry's own time with
//! integer arithmetic (a sample is `time * 44100 / clock`), so no wait
//! drifts however long the log. The chips' registers the log begins with
//! come first, before any wait. A reset writes nothing (the registers it
//! leaves follow as writes); a frame's end only marks time.

use serde::Deserialize;

use crate::audio::sound_log::{SoundChipKind, SoundEvent, SoundLog};

/// The samples a VGM's waits count per second.
pub const VGM_RATE: u64 = 44_100;

/// The VGM version written.
const VERSION: u32 = 0x0000_0171;

/// The header's length: the commands start there.
const HEADER_LEN: usize = 0x100;

/// The YM2612's DAC register.
const DAC_REGISTER: u16 = 0x2A;

/// The text a VGM's GD3 tag carries, in English; its Japanese fields are
/// left empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Gd3Tag {
    pub track: String,
    pub game: String,
    pub system: String,
    pub author: String,
    pub date: String,
    pub ripper: String,
    pub notes: String,
}

/// How a VGM holds the YM2612's DAC writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum DacForm {
    /// As register writes, three bytes each.
    #[default]
    Raw,
    /// Their bytes in a data bank at the start, each write one byte that
    /// also holds a short wait after it.
    Bank,
}

/// What a VGM is written with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct VgmOptions {
    pub tag: Gd3Tag,
    pub dac: DacForm,
    /// The entry the file loops back to, `None` for no loop.
    pub loop_entry: Option<usize>,
    /// The frame rate the header names (60 or 50), 0 for none.
    pub frame_rate: u32,
}

/// Why a log cannot be written as a VGM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VgmError {
    /// An entry names a chip the list does not have.
    UnknownChip(u8),
    /// A chip VGM 1.71 has no command for.
    UnsupportedChip(SoundChipKind),
    /// A clock of zero, which no time can be read in.
    NoClock,
}

impl std::fmt::Display for VgmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VgmError::UnknownChip(chip) => {
                write!(f, "the log names chip {chip}, which it lists not")
            }
            VgmError::UnsupportedChip(kind) => write!(f, "VGM 1.71 has no command for {kind:?}"),
            VgmError::NoClock => write!(f, "the log's clock is 0 Hz"),
        }
    }
}

impl std::error::Error for VgmError {}

/// The commands, the waits between them, and where the loop starts.
struct Commands {
    bytes: Vec<u8>,
    /// The VGM sample reached.
    at: u64,
    /// Where the last DAC write from the bank is, while a wait can still
    /// fold into it.
    last_bank_write: Option<usize>,
}

impl Commands {
    /// Wait until VGM sample `until`.
    fn wait_until(&mut self, until: u64) {
        let mut left = until.saturating_sub(self.at);
        self.at = self.at.max(until);
        if left == 0 {
            return;
        }
        if let Some(at) = self.last_bank_write.take() {
            let folded = left.min(15);
            self.bytes[at] = 0x80 | folded as u8;
            left -= folded;
        }
        while left > 0 {
            let wait = left.min(0xFFFF);
            match wait {
                1..=16 => self.bytes.push(0x70 | (wait - 1) as u8),
                735 => self.bytes.push(0x62),
                882 => self.bytes.push(0x63),
                _ => {
                    self.bytes.push(0x61);
                    self.bytes.extend_from_slice(&(wait as u16).to_le_bytes());
                }
            }
            left -= wait;
        }
    }

    fn push(&mut self, command: &[u8]) {
        self.last_bank_write = None;
        self.bytes.extend_from_slice(command);
    }
}

/// `log` as a VGM 1.71 file's bytes, `chips` the chips its entries name.
pub fn encode_vgm(
    log: &SoundLog,
    chips: &[SoundChipKind],
    options: &VgmOptions,
) -> Result<Vec<u8>, VgmError> {
    if log.clock_hz == 0 {
        return Err(VgmError::NoClock);
    }
    for &kind in chips {
        if kind == SoundChipKind::SDsp {
            return Err(VgmError::UnsupportedChip(kind));
        }
    }
    let kind_of = |chip: u8| {
        chips
            .get(usize::from(chip))
            .copied()
            .ok_or(VgmError::UnknownChip(chip))
    };
    let sample_of =
        |time: u64| (u128::from(time) * u128::from(VGM_RATE) / u128::from(log.clock_hz)) as u64;
    let is_dac = |kind: SoundChipKind, register: u16| {
        matches!(kind, SoundChipKind::Ym2612 { .. }) && register == DAC_REGISTER
    };

    let mut commands = Commands {
        bytes: Vec::new(),
        at: 0,
        last_bank_write: None,
    };
    let bank = options.dac == DacForm::Bank;
    if bank {
        let mut data = Vec::new();
        for entry in &log.entries {
            if let SoundEvent::Write { register, value } = entry.event {
                if is_dac(kind_of(entry.chip)?, register) {
                    data.push(value);
                }
            }
        }
        commands.push(&[0x67, 0x66, 0x00]);
        commands.push(&(data.len() as u32).to_le_bytes());
        commands.push(&data);
        commands.push(&[0xE0, 0, 0, 0, 0]);
    }

    let mut loop_at: Option<(usize, u64)> = None;
    let mut bank_offset = 0u32;
    for (index, entry) in log.entries.iter().enumerate() {
        commands.wait_until(sample_of(entry.time));
        if options.loop_entry == Some(index) {
            loop_at = Some((commands.bytes.len(), commands.at));
            if bank {
                commands.push(&[0xE0]);
                commands.push(&bank_offset.to_le_bytes());
            }
        }
        let SoundEvent::Write { register, value } = entry.event else {
            continue;
        };
        match kind_of(entry.chip)? {
            kind @ SoundChipKind::Ym2612 { .. } if bank && is_dac(kind, register) => {
                commands.push(&[0x80]);
                commands.last_bank_write = Some(commands.bytes.len() - 1);
                bank_offset += 1;
            }
            SoundChipKind::Ym2612 { .. } => {
                let command = if register >> 8 == 0 { 0x52 } else { 0x53 };
                commands.push(&[command, register as u8, value]);
            }
            SoundChipKind::Sn76489 { .. } => commands.push(&[0x50, value]),
            SoundChipKind::SDsp => unreachable!("refused above"),
        }
    }
    commands.push(&[0x66]);
    let total = commands.at;

    let mut out = vec![0u8; HEADER_LEN];
    out.extend_from_slice(&commands.bytes);
    let gd3_at = out.len();
    out.extend_from_slice(&gd3(&options.tag));

    let set = |out: &mut Vec<u8>, at: usize, value: u32| {
        out[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    out[..4].copy_from_slice(b"Vgm ");
    let len = out.len() as u32;
    set(&mut out, 0x04, len - 4);
    set(&mut out, 0x08, VERSION);
    for &kind in chips {
        match kind {
            SoundChipKind::Sn76489 { clock_hz } => {
                set(&mut out, 0x0C, clock_hz);
                // The Mega Drive's PSG: its noise taps and the width of its
                // shift register.
                out[0x28..0x2A].copy_from_slice(&0x0009u16.to_le_bytes());
                out[0x2A] = 16;
            }
            SoundChipKind::Ym2612 { clock_hz } => set(&mut out, 0x2C, clock_hz),
            SoundChipKind::SDsp => {}
        }
    }
    set(&mut out, 0x14, (gd3_at - 0x14) as u32);
    set(&mut out, 0x18, total as u32);
    if let Some((at, sample)) = loop_at {
        set(&mut out, 0x1C, (HEADER_LEN + at - 0x1C) as u32);
        set(&mut out, 0x20, (total - sample) as u32);
    }
    set(&mut out, 0x24, options.frame_rate);
    set(&mut out, 0x34, (HEADER_LEN - 0x34) as u32);
    Ok(out)
}

/// The GD3 tag: its eleven texts in UTF-16LE, each ended by a zero.
fn gd3(tag: &Gd3Tag) -> Vec<u8> {
    let texts = [
        &tag.track,
        "",
        &tag.game,
        "",
        &tag.system,
        "",
        &tag.author,
        "",
        &tag.date,
        &tag.ripper,
        &tag.notes,
    ];
    let mut body = Vec::new();
    for text in texts {
        for unit in text.encode_utf16().chain([0]) {
            body.extend_from_slice(&unit.to_le_bytes());
        }
    }
    let mut out = b"Gd3 ".to_vec();
    out.extend_from_slice(&0x0000_0100u32.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// `bytes` gzipped: a `.vgz` file of a VGM's bytes.
#[cfg(feature = "vgz")]
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder
        .write_all(bytes)
        .expect("a buffer in memory takes every byte");
    encoder
        .finish()
        .expect("a buffer in memory takes every byte")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::sound_log::SoundLogEntry;

    const YM: SoundChipKind = SoundChipKind::Ym2612 {
        clock_hz: 7_670_453,
    };
    const PSG: SoundChipKind = SoundChipKind::Sn76489 {
        clock_hz: 3_579_545,
    };

    fn write(time: u64, chip: u8, register: u16, value: u8) -> SoundLogEntry {
        SoundLogEntry {
            time,
            chip,
            event: SoundEvent::Write { register, value },
        }
    }

    /// A log on a clock of 44100 Hz, so times are VGM samples.
    fn log(entries: Vec<SoundLogEntry>) -> SoundLog {
        SoundLog {
            clock_hz: VGM_RATE,
            entries,
            image_len: 0,
            interrupted: false,
        }
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn a_vgm_is_its_header_its_commands_and_its_tag_byte_for_byte() {
        let log = log(vec![
            write(0, 0, 0x22, 0x08),
            write(0, 1, 0, 0x9F),
            write(10, 0, 0x1B4, 0xC0),
            SoundLogEntry {
                time: 745,
                chip: 0,
                event: SoundEvent::FrameEnd,
            },
            SoundLogEntry {
                time: 745,
                chip: 0,
                event: SoundEvent::Reset,
            },
            write(100_000, 1, 0, 0x80),
        ]);
        let options = VgmOptions {
            tag: Gd3Tag {
                track: "T".into(),
                ..Gd3Tag::default()
            },
            frame_rate: 60,
            ..VgmOptions::default()
        };
        let vgm = encode_vgm(&log, &[YM, PSG], &options).unwrap();
        let expected = [
            0x52, 0x22, 0x08, // the first chip's registers: part 0, register 0x22
            0x50, 0x9F, // the PSG's
            0x79, // 10 samples
            0x53, 0xB4, 0xC0, // part 1
            0x62, // 735 samples; the frame's end and the reset write nothing
            0x61, 0xFF, 0xFF, // 99,255 samples: 65,535, then 33,720
            0x61, 0xB8, 0x83, 0x50, 0x80, 0x66,
        ];
        assert_eq!(&vgm[HEADER_LEN..HEADER_LEN + expected.len()], &expected[..]);
        assert_eq!(&vgm[..4], b"Vgm ");
        assert_eq!(u32_at(&vgm, 0x04) as usize, vgm.len() - 4);
        assert_eq!(u32_at(&vgm, 0x08), 0x171);
        assert_eq!(u32_at(&vgm, 0x0C), 3_579_545);
        assert_eq!(u32_at(&vgm, 0x18), 100_000, "the samples in all");
        assert_eq!(u32_at(&vgm, 0x1C), 0, "no loop");
        assert_eq!(u32_at(&vgm, 0x24), 60);
        assert_eq!(&vgm[0x28..0x2B], &[0x09, 0x00, 16]);
        assert_eq!(u32_at(&vgm, 0x2C), 7_670_453);
        assert_eq!(u32_at(&vgm, 0x34), 0xCC);
        let gd3_at = 0x14 + u32_at(&vgm, 0x14) as usize;
        assert_eq!(gd3_at, HEADER_LEN + expected.len());
        assert_eq!(&vgm[gd3_at..gd3_at + 4], b"Gd3 ");
        assert_eq!(u32_at(&vgm, gd3_at + 4), 0x100);
        let body = u32_at(&vgm, gd3_at + 8) as usize;
        assert_eq!(
            body,
            2 * (1 + 1) + 2 * 10,
            "T and its zero, then ten empty texts"
        );
        assert_eq!(&vgm[gd3_at + 12..gd3_at + 16], &[b'T', 0, 0, 0]);
        assert_eq!(gd3_at + 12 + body, vgm.len());
    }

    #[test]
    fn waits_are_worked_out_from_each_entry_s_own_time() {
        // A clock where a sample is not a whole number of ticks: 100,000
        // ticks a second, 0.441 samples a tick.
        let mut entries: Vec<SoundLogEntry> = (0..1000).map(|n| write(n * 3, 1, 0, 0x90)).collect();
        entries.push(write(1_000_000, 1, 0, 0x9F));
        let log = SoundLog {
            clock_hz: 100_000,
            ..log(entries)
        };
        let vgm = encode_vgm(&log, &[YM, PSG], &VgmOptions::default()).unwrap();
        assert_eq!(u32_at(&vgm, 0x18), 441_000, "ten seconds, to the sample");
    }

    #[test]
    fn the_dac_bank_holds_the_dac_s_bytes_and_a_wait_folds_into_each() {
        let log = log(vec![
            write(0, 0, 0x2B, 0x80),
            write(0, 0, 0x2A, 0x11),
            write(3, 0, 0x2A, 0x22),
            write(30, 0, 0x2A, 0x33),
            write(30, 1, 0, 0x9F),
        ]);
        let options = VgmOptions {
            dac: DacForm::Bank,
            loop_entry: Some(2),
            ..VgmOptions::default()
        };
        let vgm = encode_vgm(&log, &[YM, PSG], &options).unwrap();
        let expected = [
            0x67, 0x66, 0x00, 3, 0, 0, 0, 0x11, 0x22, 0x33, // the bank
            0xE0, 0, 0, 0, 0, // its start
            0x52, 0x2B, 0x80, // the DAC on
            0x83, // the first byte, then 3 samples
            0xE0, 1, 0, 0, 0, // the loop: the bank from its second byte
            0x8F, 0x7B, // the second byte, 15 samples, then 12
            0x80, // the third
            0x50, 0x9F, 0x66,
        ];
        assert_eq!(&vgm[HEADER_LEN..HEADER_LEN + expected.len()], &expected[..]);
        assert_eq!(
            u32_at(&vgm, 0x1C) as usize + 0x1C,
            HEADER_LEN + 19,
            "the loop's command"
        );
        assert_eq!(u32_at(&vgm, 0x20), 27, "the samples from the loop on");
    }

    /// A mailbox's write is stepped over as a key is: the file of a log with
    /// them is the file of the same log with a key-off in each one's place,
    /// the loop marked at a mailbox's write where its entry is one; and one
    /// at the time of the entry before it leaves the file as it was.
    #[test]
    fn a_mailbox_s_write_is_stepped_over_as_a_key_is() {
        let mailbox = |time, value| SoundLogEntry {
            time,
            chip: 0,
            event: SoundEvent::Mailbox { port: 3, value },
        };
        let key_off = |time| SoundLogEntry {
            time,
            chip: 0,
            event: SoundEvent::KeyOff { channel: 0 },
        };
        let writes = vec![
            write(0, 0, 0x22, 0x08),
            write(10, 1, 0, 0x9F),
            write(400, 0, 0x28, 0xF0),
        ];
        let mut with_mailbox = writes.clone();
        with_mailbox.insert(1, mailbox(5, 0x11));
        with_mailbox.insert(3, mailbox(200, 0x12));
        let mut with_keys = writes.clone();
        with_keys.insert(1, key_off(5));
        with_keys.insert(3, key_off(200));
        let options = VgmOptions {
            loop_entry: Some(3),
            ..VgmOptions::default()
        };
        let vgm = encode_vgm(&log(with_mailbox), &[YM, PSG], &options).unwrap();
        assert_eq!(
            vgm,
            encode_vgm(&log(with_keys), &[YM, PSG], &options).unwrap()
        );
        assert_eq!(u32_at(&vgm, 0x20), 200, "the loop from the mailbox's write");
        assert_eq!(u32_at(&vgm, 0x18), 400, "the samples in all");
        let mut beside = writes.clone();
        beside.insert(2, mailbox(10, 0x13));
        assert_eq!(
            encode_vgm(&log(beside), &[YM, PSG], &VgmOptions::default()).unwrap(),
            encode_vgm(&log(writes), &[YM, PSG], &VgmOptions::default()).unwrap()
        );
    }

    #[cfg(feature = "vgz")]
    #[test]
    fn a_vgz_is_the_vgm_gzipped() {
        use std::io::Read;
        let log = log(vec![write(0, 0, 0x22, 0x08), write(735, 1, 0, 0x9F)]);
        let vgm = encode_vgm(&log, &[YM, PSG], &VgmOptions::default()).unwrap();
        let vgz = gzip(&vgm);
        assert_eq!(&vgz[..2], &[0x1F, 0x8B], "a gzip stream");
        let mut back = Vec::new();
        flate2::read::GzDecoder::new(&vgz[..])
            .read_to_end(&mut back)
            .unwrap();
        assert_eq!(back, vgm);
    }

    #[test]
    fn a_chip_vgm_has_no_command_for_is_refused() {
        let log = log(vec![write(0, 0, 0x4C, 1)]);
        assert_eq!(
            encode_vgm(&log, &[SoundChipKind::SDsp], &VgmOptions::default()),
            Err(VgmError::UnsupportedChip(SoundChipKind::SDsp))
        );
        assert_eq!(
            encode_vgm(&log, &[], &VgmOptions::default()),
            Err(VgmError::UnknownChip(0))
        );
        let none = SoundLog {
            clock_hz: 0,
            ..log.clone()
        };
        assert_eq!(
            encode_vgm(&none, &[YM], &VgmOptions::default()),
            Err(VgmError::NoClock)
        );
    }
}
