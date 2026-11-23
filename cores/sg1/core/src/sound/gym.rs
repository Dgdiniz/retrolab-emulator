//! The GYM writer: a sound log of the Mega Drive as a GYM file, the
//! format that counts time in frames.
//!
//! Each YM2612 write is a command of its part (`0x01` part 0, `0x02` part
//! 1, then the register and the value), each PSG byte a `0x03` command,
//! and each frame's end a `0x00`, a wait of one frame. A write's place
//! within its frame is not kept: a player plays a frame's writes at its
//! start. A reset writes nothing (the registers it leaves follow as
//! writes). The 428-byte GYMX header is optional.

use retro_core::audio::{SoundChipKind, SoundEvent, SoundLog};
use serde::Deserialize;

/// A GYMX header's texts, each cut to its field (32 bytes, the comment
/// 256), and the frame a player loops back to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct GymHeader {
    pub song: String,
    pub game: String,
    pub publisher: String,
    pub emulator: String,
    pub dumper: String,
    pub comment: String,
    /// The frame, counted from 1, a player loops back to; 0 for none.
    pub loop_frame: u32,
}

/// The GYMX header's length.
pub const GYMX_HEADER_LEN: usize = 428;

/// `log` as a GYM file's bytes, `chips` the chips its entries name (the
/// YM2612 and the PSG), with a GYMX header when `header` is given.
pub fn encode_gym(
    log: &SoundLog,
    chips: &[SoundChipKind],
    header: Option<&GymHeader>,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    if let Some(header) = header {
        out.extend_from_slice(b"GYMX");
        for (text, len) in [
            (&header.song, 32),
            (&header.game, 32),
            (&header.publisher, 32),
            (&header.emulator, 32),
            (&header.dumper, 32),
            (&header.comment, 256),
        ] {
            let mut field = text.as_bytes()[..text.len().min(len)].to_vec();
            field.resize(len, 0);
            out.extend_from_slice(&field);
        }
        out.extend_from_slice(&header.loop_frame.to_le_bytes());
        // Not packed.
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    for entry in &log.entries {
        let SoundEvent::Write { register, value } = entry.event else {
            if entry.event == SoundEvent::FrameEnd {
                out.push(0x00);
            }
            continue;
        };
        match chips.get(usize::from(entry.chip)) {
            Some(SoundChipKind::Ym2612 { .. }) => {
                let part = if register >> 8 == 0 { 0x01 } else { 0x02 };
                out.extend_from_slice(&[part, register as u8, value]);
            }
            Some(SoundChipKind::Sn76489 { .. }) => out.extend_from_slice(&[0x03, value]),
            Some(kind) => return Err(format!("GYM has no command for {kind:?}")),
            None => {
                return Err(format!(
                    "the log names chip {}, which it lists not",
                    entry.chip
                ))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::audio::SoundLogEntry;

    const CHIPS: [SoundChipKind; 2] = [
        SoundChipKind::Ym2612 {
            clock_hz: 7_670_453,
        },
        SoundChipKind::Sn76489 {
            clock_hz: 3_579_545,
        },
    ];

    fn entry(time: u64, chip: u8, event: SoundEvent) -> SoundLogEntry {
        SoundLogEntry { time, chip, event }
    }

    fn log(entries: Vec<SoundLogEntry>) -> SoundLog {
        SoundLog {
            clock_hz: 53_693_175,
            entries,
            image_len: 0,
            interrupted: false,
        }
    }

    #[test]
    fn a_gym_is_its_writes_and_a_wait_a_frame_byte_for_byte() {
        let write = |register, value| SoundEvent::Write { register, value };
        let log = log(vec![
            entry(0, 0, write(0x22, 0x08)),
            entry(0, 1, write(0, 0x9F)),
            entry(40, 0, write(0x1B4, 0xC0)),
            entry(895_000, 0, SoundEvent::FrameEnd),
            entry(895_000, 0, SoundEvent::Reset),
            entry(900_000, 0, write(0x28, 0xF0)),
            entry(1_790_000, 0, SoundEvent::FrameEnd),
        ]);
        let gym = encode_gym(&log, &CHIPS, None).unwrap();
        assert_eq!(
            gym,
            [0x01, 0x22, 0x08, 0x03, 0x9F, 0x02, 0xB4, 0xC0, 0x00, 0x01, 0x28, 0xF0, 0x00]
        );
    }

    #[test]
    fn a_gymx_header_holds_its_texts_cut_to_their_fields_and_the_loop() {
        let header = GymHeader {
            song: "Song".into(),
            game: "G".repeat(40),
            loop_frame: 7,
            ..GymHeader::default()
        };
        let gym = encode_gym(&log(Vec::new()), &CHIPS, Some(&header)).unwrap();
        assert_eq!(gym.len(), GYMX_HEADER_LEN);
        assert_eq!(&gym[..8], b"GYMXSong");
        assert_eq!(gym[8], 0);
        assert_eq!(&gym[36..68], "G".repeat(32).as_bytes());
        assert_eq!(&gym[420..424], &7u32.to_le_bytes());
        assert_eq!(&gym[424..428], &[0; 4]);
    }

    #[test]
    fn a_chip_gym_has_no_command_for_is_refused() {
        let log = log(vec![entry(
            0,
            0,
            SoundEvent::Write {
                register: 0,
                value: 0,
            },
        )]);
        assert!(encode_gym(&log, &[SoundChipKind::SDsp], None).is_err());
        assert!(encode_gym(&log, &[], None).is_err());
    }
}
