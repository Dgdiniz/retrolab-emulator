//! The SPC file: the APU's state as an SPC player loads it and runs it
//! (version 0.30), with an ID666 tag and, for a text longer than the tag
//! has room for, an xid6 chunk.
//!
//! The file holds the SPC700's registers, the 64 KiB of ARAM, the S-DSP's
//! 128 registers and the 64 bytes of ARAM the IPL ROM sits over. What else
//! the APU holds (where each voice is in its sample, its envelope's level,
//! the timers' dividers) has no place in it: a note sounding when the file
//! was taken is not heard in a player, which keys on the voices KON names
//! when it starts and plays on from the program's next key-on.
//!
//! ARAM's `$F0-$FF` are the APU's registers, whose values the SPC700 never
//! writes into ARAM; the file holds what a player loads into them, rebuilt
//! from the APU's state: `$F0` TEST's power-on value 0x0A (the core keeps
//! no TEST); `$F1` CONTROL's IPL bit with the three timers' enables (its
//! port-clearing bits act when written and are left clear); `$F2` the DSP
//! address and `$F3` the register it selects; `$F4-$F7` the ports as the
//! SPC700 reads them; `$F8-$F9` the two auxiliary registers; `$FA-$FC` the
//! timers' targets and `$FD-$FF` their 4-bit outputs.
//!
//! | Offset | What |
//! |---|---|
//! | 0x00-0x20 | `SNES-SPC700 Sound File Data v0.30` |
//! | 0x21-0x22 | 26, 26 |
//! | 0x23 | 26 with an ID666 tag, 27 without |
//! | 0x24 | 30, the minor version |
//! | 0x25-0x2B | PC (low byte first), A, X, Y, PSW, SP |
//! | 0x2E-0xD2 | the ID666 tag, in its text form |
//! | 0x100-0x100FF | ARAM, `$F0-$FF` rebuilt |
//! | 0x10100-0x1017F | the S-DSP's registers |
//! | 0x101C0-0x101FF | ARAM `$FFC0-$FFFF`, under the IPL ROM |
//! | 0x10200 | the xid6 chunk, when a text is longer than its place |

use serde::{Deserialize, Serialize};

use crate::savestate::ApuSaveState;

/// The bytes of an SPC file before any xid6 chunk.
pub const SPC_LEN: usize = 0x10200;

/// What the file is, its first 33 bytes.
const IDENT: &[u8; 33] = b"SNES-SPC700 Sound File Data v0.30";

/// The text an SPC's ID666 tag holds; a default tag is none. Each text is
/// written in ASCII, any other character as `?`. A text longer than its
/// place in the tag is cut there and written whole (to 255 bytes) in the
/// file's xid6 chunk. The date is written as given: the tag's form is
/// `MM/DD/YYYY`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Id666 {
    /// The song's title (32 bytes in the tag).
    pub title: String,
    /// The game's (32).
    pub game: String,
    /// Who made the file (16).
    pub dumper: String,
    /// A comment (32).
    pub comment: String,
    /// The day the file was made (11).
    pub date: String,
    /// The seconds a player plays before it fades out, at most 999; 0 for
    /// none given.
    pub seconds: u32,
    /// The fade's length in milliseconds, at most 99999; 0 for none given.
    pub fade_ms: u32,
    /// The song's composer (32).
    pub artist: String,
}

impl Id666 {
    /// Whether the tag holds nothing, so the file is written without one.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A tag's text field: its place in the file, its length there, and the
/// id of its xid6 sub-chunk.
struct TextField {
    offset: usize,
    len: usize,
    xid6: u8,
}

const TITLE: TextField = TextField {
    offset: 0x2E,
    len: 32,
    xid6: 0x01,
};
const GAME: TextField = TextField {
    offset: 0x4E,
    len: 32,
    xid6: 0x02,
};
const DUMPER: TextField = TextField {
    offset: 0x6E,
    len: 16,
    xid6: 0x04,
};
const COMMENT: TextField = TextField {
    offset: 0x7E,
    len: 32,
    xid6: 0x07,
};
const ARTIST: TextField = TextField {
    offset: 0xB1,
    len: 32,
    xid6: 0x03,
};
const DATE_AT: usize = 0x9E;
const DATE_LEN: usize = 11;
const SECONDS_AT: usize = 0xA9;
const FADE_AT: usize = 0xAC;

/// `text` in the tag's characters: ASCII, any other character as `?`.
fn ascii(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c {
            ' '..='~' => c as u8,
            _ => b'?',
        })
        .collect()
}

/// Write `bytes`, cut to `len`, at `at` (the rest of the place stays 0).
fn put(file: &mut [u8], at: usize, len: usize, bytes: &[u8]) {
    let n = bytes.len().min(len);
    file[at..at + n].copy_from_slice(&bytes[..n]);
}

/// The values a player loads into `$F0-$FF`, rebuilt from the APU's state
/// (the module's documentation says each one).
pub fn register_page(apu: &ApuSaveState) -> [u8; 16] {
    let timers = [&apu.timer0, &apu.timer1, &apu.timer2];
    let mut page = [0u8; 16];
    page[0x0] = 0x0A;
    page[0x1] = (apu.control & 0x80)
        | timers
            .iter()
            .enumerate()
            .filter(|(_, timer)| timer.enabled)
            .fold(0, |bits, (index, _)| bits | 1 << index);
    page[0x2] = apu.dsp.addr;
    page[0x3] = apu.dsp.regs[usize::from(apu.dsp.addr & 0x7F)];
    page[0x4..0x8].copy_from_slice(&apu.ports_cpu_to_apu);
    page[0x8..0xA].copy_from_slice(&apu.aux);
    for (index, timer) in timers.iter().enumerate() {
        page[0xA + index] = timer.target;
        page[0xD + index] = timer.output & 0x0F;
    }
    page
}

/// The xid6 chunk of the texts longer than their place in the tag: `xid6`,
/// the size of what follows, then a string sub-chunk a text (its id, type
/// 1, its length with the closing zero, the bytes padded to four); empty
/// when every text fits.
fn xid6(tag: &Id666) -> Vec<u8> {
    let mut subchunks = Vec::new();
    for (field, text) in [
        (TITLE, &tag.title),
        (GAME, &tag.game),
        (ARTIST, &tag.artist),
        (DUMPER, &tag.dumper),
        (COMMENT, &tag.comment),
    ] {
        let mut bytes = ascii(text);
        if bytes.len() <= field.len {
            continue;
        }
        bytes.truncate(255);
        bytes.push(0);
        subchunks.extend_from_slice(&[field.xid6, 1]);
        subchunks.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        subchunks.extend_from_slice(&bytes);
        subchunks.resize(subchunks.len().next_multiple_of(4), 0);
    }
    if subchunks.is_empty() {
        return subchunks;
    }
    let mut chunk = b"xid6".to_vec();
    chunk.extend_from_slice(&(subchunks.len() as u32).to_le_bytes());
    chunk.extend_from_slice(&subchunks);
    chunk
}

/// The SPC file of the APU's state `apu`, tagged with `tag` (none when it
/// is empty).
pub fn build_spc(apu: &ApuSaveState, tag: &Id666) -> Vec<u8> {
    let mut file = vec![0u8; SPC_LEN];
    file[..IDENT.len()].copy_from_slice(IDENT);
    file[0x21] = 26;
    file[0x22] = 26;
    file[0x23] = if tag.is_empty() { 27 } else { 26 };
    file[0x24] = 30;
    let cpu = &apu.spc700;
    file[0x25..0x27].copy_from_slice(&cpu.pc.to_le_bytes());
    file[0x27..0x2C].copy_from_slice(&[cpu.a, cpu.x, cpu.y, cpu.psw, cpu.sp]);
    if !tag.is_empty() {
        for (field, text) in [
            (TITLE, &tag.title),
            (GAME, &tag.game),
            (DUMPER, &tag.dumper),
            (COMMENT, &tag.comment),
            (ARTIST, &tag.artist),
        ] {
            put(&mut file, field.offset, field.len, &ascii(text));
        }
        put(&mut file, DATE_AT, DATE_LEN - 1, &ascii(&tag.date));
        // A length of 0 is left out, which a player reads as none given.
        if tag.seconds > 0 {
            let seconds = tag.seconds.min(999).to_string();
            put(&mut file, SECONDS_AT, 3, seconds.as_bytes());
        }
        if tag.fade_ms > 0 {
            let fade = tag.fade_ms.min(99_999).to_string();
            put(&mut file, FADE_AT, 5, fade.as_bytes());
        }
    }
    file[0x100..0x10100].copy_from_slice(&apu.aram[..0x10000]);
    file[0x1F0..0x200].copy_from_slice(&register_page(apu));
    file[0x10100..0x10180].copy_from_slice(&apu.dsp.regs[..0x80]);
    file[0x101C0..0x10200].copy_from_slice(&apu.aram[0xFFC0..0x10000]);
    file.extend_from_slice(&xid6(tag));
    file
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apu::Apu;
    use retro_core::hash::fnv1a64;

    /// An APU state with a value of its own in every place the file reads.
    fn state() -> ApuSaveState {
        let mut apu = Apu::new().to_savestate();
        apu.spc700.pc = 0x1234;
        apu.spc700.a = 0xA1;
        apu.spc700.x = 0xB2;
        apu.spc700.y = 0xC3;
        apu.spc700.psw = 0xD4;
        apu.spc700.sp = 0xE5;
        for (i, byte) in apu.aram.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(7) ^ (i >> 8) as u8;
        }
        apu.ports_cpu_to_apu = [0x11, 0x22, 0x33, 0x44];
        apu.ports_apu_to_cpu = [0x55, 0x66, 0x77, 0x88];
        apu.aux = [0x99, 0xAA];
        apu.timer0.enabled = true;
        apu.timer0.target = 0x10;
        apu.timer0.output = 0x13;
        apu.timer1.enabled = false;
        apu.timer1.target = 0x20;
        apu.timer1.output = 0x0E;
        apu.timer2.enabled = true;
        apu.timer2.target = 0x30;
        apu.timer2.output = 0x05;
        apu.control = 0x87;
        for (i, reg) in apu.dsp.regs.iter_mut().enumerate() {
            *reg = 0xFF - i as u8;
        }
        apu.dsp.addr = 0x4C;
        apu
    }

    #[test]
    fn the_file_holds_the_state_at_its_offsets() {
        let apu = state();
        let file = build_spc(&apu, &Id666::default());
        assert_eq!(file.len(), SPC_LEN);
        assert_eq!(&file[..33], IDENT);
        assert_eq!(&file[0x21..0x25], &[26, 26, 27, 30], "no tag");
        assert_eq!(
            &file[0x25..0x2E],
            &[0x34, 0x12, 0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0, 0]
        );
        assert!(file[0x2E..0x100].iter().all(|&b| b == 0), "no tag");
        assert_eq!(&file[0x100..0x1F0], &apu.aram[..0xF0]);
        assert_eq!(&file[0x200..0x10100], &apu.aram[0x100..]);
        assert_eq!(&file[0x10100..0x10180], &apu.dsp.regs[..]);
        assert!(file[0x10180..0x101C0].iter().all(|&b| b == 0));
        assert_eq!(&file[0x101C0..], &apu.aram[0xFFC0..]);
        // $F0-$FF: TEST; CONTROL's IPL bit and timers 0 and 2 enabled;
        // DSPADDR and the register it names (KON, register 0x4C, holds
        // 0xFF - 0x4C); the ports the SPC700 reads; the aux; the targets
        // and the 4-bit outputs.
        assert_eq!(
            &file[0x1F0..0x200],
            &[
                0x0A, 0x85, 0x4C, 0xB3, 0x11, 0x22, 0x33, 0x44, 0x99, 0xAA, 0x10, 0x20, 0x30, 0x03,
                0x0E, 0x05
            ]
        );
        assert_eq!(fnv1a64(&file), 0x22E9_D032_9EBA_495D, "pinned");
    }

    #[test]
    fn the_tag_is_written_in_its_text_form_and_long_texts_in_xid6() {
        let tag = Id666 {
            title: "A title that is longer than thirty-two bytes".to_string(),
            game: "Game".to_string(),
            dumper: "dumper".to_string(),
            comment: "caf\u{e9}".to_string(),
            date: "11/22/2026".to_string(),
            seconds: 180,
            fade_ms: 1_000_000,
            artist: "Composer".to_string(),
        };
        let file = build_spc(&state(), &tag);
        assert_eq!(file[0x23], 26, "a tag");
        assert_eq!(&file[0x2E..0x4E], &b"A title that is longer than thir"[..]);
        assert_eq!(&file[0x4E..0x53], b"Game\0");
        assert_eq!(&file[0x6E..0x75], b"dumper\0");
        assert_eq!(&file[0x7E..0x83], b"caf?\0");
        assert_eq!(&file[0x9E..0xA9], b"11/22/2026\0");
        assert_eq!(&file[0xA9..0xAC], b"180");
        assert_eq!(&file[0xAC..0xB1], b"99999");
        assert_eq!(&file[0xB1..0xBA], b"Composer\0");
        let chunk = &file[SPC_LEN..];
        assert_eq!(&chunk[..4], b"xid6");
        let title = b"A title that is longer than thirty-two bytes\0";
        let padded = title.len().next_multiple_of(4);
        assert_eq!(&chunk[4..8], &((4 + padded) as u32).to_le_bytes());
        assert_eq!(&chunk[8..12], &[0x01, 1, title.len() as u8, 0]);
        assert_eq!(&chunk[12..12 + title.len()], title);
        assert_eq!(chunk.len(), 12 + padded);
        assert_eq!(fnv1a64(&file), 0xC762_3455_039D_2C09, "pinned");
    }
}
