//! The echo unit, and the final mix.
//!
//! The chip keeps a ring of past stereo samples in ARAM, starting at the
//! page ESA names, as long as EDL says (EDL x 2048 bytes; 4 when EDL is
//! 0), and each sample reads one frame from it into an eight-deep
//! history, filters the history with the eight FIR coefficients, mixes
//! the result into the output through the echo volume, and writes the
//! voices' echo sum, plus the filtered result scaled by the feedback
//! volume, back into the ring where the frame was read. A flag in FLG
//! turns the write back off; the read, the filter and the mix go on.
//!
//! The filter runs over clocks 22 to 25 of the sample, after the frame's
//! two halves are read in; the chip adds the first seven taps and then
//! the eighth in 16 bits. The feedback and the output follow at 26 and
//! 27; the ring steps and the frame is written back at 29 and 30, under
//! the flags latched at 28.
//!
//! While a voice is muted from the debugger, the speakers hear a mix of
//! their own ([`SpeakersMix`]), kept beside the chip's through the same
//! steps, which leaves the muted voices out of its sums and of its echo;
//! the chip's sums, its echo and the ring in ARAM are those of the chip
//! with nothing muted.
//!
//! Written from fullsnes's echo registers and Anomie's notes on the
//! clock-by-clock order; checked against bsnes-plus.

use super::registers::{
    DSP_REG_COUNT, EDL, EFB, ESA, EVOLL, EVOLR, FIR, FLG, FLG_ECHO_OFF, FLG_MUTE, MVOLL, MVOLR,
};
use super::{clamp16, SDsp};

/// Frames of history the filter runs over.
pub(super) const HISTORY_LEN: usize = 8;

/// One tap of the filter over `history`: the frame `tap + 1` slots past
/// the head, so tap 7 is the frame just read, through its coefficient.
#[inline]
fn tap_of(regs: &[u8; DSP_REG_COUNT], history: &[i16; HISTORY_LEN], head: u8, tap: usize) -> i32 {
    let slot = (usize::from(head) + tap + 1) % HISTORY_LEN;
    let coefficient = i32::from(regs[FIR + tap * 0x10] as i8);
    (i32::from(history[slot]) * coefficient) >> 6
}

/// The filter over `history`: the first seven taps add up in full width
/// and the sum is cut to 16 bits; the eighth tap is cut on its own and
/// added; the result is clamped and has bit 0 cleared.
#[inline]
fn filtered(regs: &[u8; DSP_REG_COUNT], history: &[i16; HISTORY_LEN], head: u8) -> i32 {
    let seven: i32 = (0..7).map(|tap| tap_of(regs, history, head, tap)).sum();
    let eighth = i32::from(tap_of(regs, history, head, 7) as i16);
    let sum = i32::from(seven as i16) + eighth;
    clamp16(sum) & !1
}

/// A main sum through the main volume plus a filtered echo through the
/// echo volume, each cut to 16 bits, the sum clamped.
#[inline]
fn mixed_out(regs: &[u8; DSP_REG_COUNT], main_sum: i32, echo_return: i32, channel: usize) -> i32 {
    let main_volume = i32::from(regs[[MVOLL, MVOLR][channel]] as i8);
    let echo_volume = i32::from(regs[[EVOLL, EVOLR][channel]] as i8);
    let main = i32::from(((main_sum * main_volume) >> 7) as i16);
    let echo = i32::from(((echo_return * echo_volume) >> 7) as i16);
    clamp16(main + echo)
}

/// An echo sum with a filtered echo fed back into it through EFB.
#[inline]
fn fed_back(regs: &[u8; DSP_REG_COUNT], echo_sum: i32, echo_return: i32) -> i32 {
    let feedback = i32::from(regs[EFB] as i8);
    let fed = i32::from(((echo_return * feedback) >> 7) as i16);
    clamp16(echo_sum + fed) & !1
}

/// The 16-bit word of ARAM at `at`, little-endian, wrapping at 64 KiB.
#[inline]
fn aram_word(aram: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([aram[at], aram[(at + 1) & 0xffff]])
}

/// What the speakers hear while a voice is muted from the debugger: a mix
/// of the voices that are not, with an echo of its own. The chip mixes a
/// muted voice as any other, so nothing it holds depends on a mute; this
/// mix runs beside it through the same steps, its sums fed by the voices
/// that are not muted, its echo read through the chip's filter, feedback
/// and volumes, and its sample is the one the chip outputs. It starts from
/// the chip's as the first mute begins and ends with the last mute;
/// nothing of it is saved.
#[derive(Clone)]
pub(super) struct SpeakersMix {
    main_sum: [i32; 2],
    echo_sum: [i32; 2],
    echo_return: [i32; 2],
    echo_history: [[i16; HISTORY_LEN]; 2],
    /// The speakers' echo ring, word by word of ARAM: where the chip's
    /// echo wrote a word since the mix began, the word the chip wrote and
    /// the speakers' own. The speakers read their own word while ARAM
    /// still holds the chip's there, and ARAM's otherwise (a word the
    /// echo never wrote, or one the program wrote over).
    ring: Box<[Option<(i16, i16)>]>,
}

impl std::fmt::Debug for SpeakersMix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpeakersMix")
            .field("main_sum", &self.main_sum)
            .field("echo_sum", &self.echo_sum)
            .field("echo_return", &self.echo_return)
            .field("echo_history", &self.echo_history)
            .field("ring_words", &self.ring.iter().flatten().count())
            .finish()
    }
}

impl SpeakersMix {
    /// The speakers' mix as the chip's stands: the same sums, history and
    /// filtered echo, the ring as ARAM holds it.
    pub(super) fn from_chip(dsp: &SDsp) -> Self {
        Self {
            main_sum: dsp.main_sum,
            echo_sum: dsp.echo_sum,
            echo_return: dsp.echo_return,
            echo_history: dsp.echo_history,
            ring: vec![None; 0x8000].into_boxed_slice(),
        }
    }

    /// Add a voice's amount for `channel`, through its volume, to the main
    /// sum and, when it is routed to the echo, to the echo sum, as the
    /// chip's mix adds it.
    #[inline]
    pub(super) fn mix(&mut self, channel: usize, amount: i32, echo: bool) {
        self.main_sum[channel] = clamp16(self.main_sum[channel] + amount);
        if echo {
            self.echo_sum[channel] = clamp16(self.echo_sum[channel] + amount);
        }
    }

    /// The word at `at` as the speakers' echo reads it.
    fn read(&self, at: usize, aram: &[u8]) -> i16 {
        let chip = aram_word(aram, at);
        match self.ring[at >> 1] {
            Some((written, own)) if written == chip => own,
            _ => chip,
        }
    }

    /// The speakers' half of a frame written back where the chip writes
    /// `chip_word` (`None` while the latched flags forbid the write);
    /// either way the sum starts again from zero.
    fn write_back(&mut self, channel: usize, at: usize, chip_word: Option<i16>) {
        if let Some(word) = chip_word {
            self.ring[at >> 1] = Some((word, self.echo_sum[channel] as i16));
        }
        self.echo_sum[channel] = 0;
    }

    /// The speakers' main sums.
    #[cfg(test)]
    pub(super) fn main_sum(&self) -> [i32; 2] {
        self.main_sum
    }

    /// The words of the speakers' ring that differ from the chip's.
    #[cfg(test)]
    pub(super) fn words_unlike_the_chip(&self) -> usize {
        self.ring
            .iter()
            .flatten()
            .filter(|(chip, own)| chip != own)
            .count()
    }
}

impl SDsp {
    /// ARAM address of this sample's frame for `channel`, two bytes per
    /// channel, wrapping at 64 KiB.
    #[inline]
    fn frame_address(&self, channel: usize) -> usize {
        usize::from(self.echo_pointer.wrapping_add(channel as u16 * 2))
    }

    /// Read `channel`'s half of the frame into the history, halved.
    fn read_into_history(&mut self, channel: usize, aram: &[u8]) {
        let at = self.frame_address(channel);
        let head = usize::from(self.history_head);
        self.echo_history[channel][head] = aram_word(aram, at) >> 1;
        if let Some(speakers) = self.speakers.as_deref_mut() {
            speakers.echo_history[channel][head] = speakers.read(at, aram) >> 1;
        }
    }

    /// Write `channel`'s echo sum into the frame, unless the latched flags
    /// forbid it; either way the sum starts again from zero.
    fn write_back(&mut self, channel: usize, aram: &mut [u8]) {
        let at = self.frame_address(channel);
        let word = self.echo_sum[channel] as i16;
        let written = self.echo_write_flags & FLG_ECHO_OFF == 0;
        if written {
            let [lo, hi] = word.to_le_bytes();
            aram[at] = lo;
            aram[(at + 1) & 0xffff] = hi;
        }
        if let Some(speakers) = self.speakers.as_deref_mut() {
            speakers.write_back(channel, at, written.then_some(word));
        }
        self.echo_sum[channel] = 0;
    }

    /// One tap of the chip's filter: the frame `tap + 1` slots past the
    /// head, so tap 7 is the frame just read, through its coefficient.
    #[cfg(test)]
    fn tap(&self, channel: usize, tap: usize) -> i32 {
        tap_of(
            &self.regs,
            &self.echo_history[channel],
            self.history_head,
            tap,
        )
    }

    /// The main sum through the main volume plus the filtered echo through
    /// the echo volume, each cut to 16 bits, the sum clamped.
    fn mixed(&self, channel: usize) -> i32 {
        mixed_out(
            &self.regs,
            self.main_sum[channel],
            self.echo_return[channel],
            channel,
        )
    }

    /// Clock 22: the next history slot, this sample's frame, and its left
    /// half read in.
    pub(super) fn echo_left_in(&mut self, aram: &[u8]) {
        self.history_head = ((usize::from(self.history_head) + 1) % HISTORY_LEN) as u8;
        self.echo_pointer = ((u32::from(self.echo_page) << 8) + u32::from(self.echo_offset)) as u16;
        self.read_into_history(0, aram);
    }

    /// Clock 23: the right half.
    pub(super) fn echo_right_in(&mut self, aram: &[u8]) {
        self.read_into_history(1, aram);
    }

    /// Clocks 22-25: the filter, over the chip's history and the speakers'.
    pub(super) fn echo_filter(&mut self) {
        for channel in 0..2 {
            self.echo_return[channel] =
                filtered(&self.regs, &self.echo_history[channel], self.history_head);
        }
        if let Some(speakers) = self.speakers.as_deref_mut() {
            for channel in 0..2 {
                speakers.echo_return[channel] = filtered(
                    &self.regs,
                    &speakers.echo_history[channel],
                    self.history_head,
                );
            }
        }
    }

    /// Clock 26: the left output is settled now (the right one at 27),
    /// and the filtered echo feeds back into the sums to be written.
    pub(super) fn echo_feedback(&mut self) {
        self.main_sum[0] = self.mixed(0);
        for channel in 0..2 {
            self.echo_sum[channel] = fed_back(
                &self.regs,
                self.echo_sum[channel],
                self.echo_return[channel],
            );
        }
        if let Some(speakers) = self.speakers.as_deref_mut() {
            speakers.main_sum[0] =
                mixed_out(&self.regs, speakers.main_sum[0], speakers.echo_return[0], 0);
            for channel in 0..2 {
                speakers.echo_sum[channel] = fed_back(
                    &self.regs,
                    speakers.echo_sum[channel],
                    speakers.echo_return[channel],
                );
            }
        }
    }

    /// Clock 27: the stereo sample for the DAC, silent while FLG mutes:
    /// the chip's, or the speakers' while a voice is muted.
    pub(super) fn output(&mut self) -> (i16, i16) {
        let mut left = self.main_sum[0];
        let mut right = self.mixed(1);
        self.main_sum = [0; 2];
        if let Some(speakers) = self.speakers.as_deref_mut() {
            left = speakers.main_sum[0];
            right = mixed_out(&self.regs, speakers.main_sum[1], speakers.echo_return[1], 1);
            speakers.main_sum = [0; 2];
        }
        if self.regs[FLG] & FLG_MUTE != 0 {
            (0, 0)
        } else {
            (left as i16, right as i16)
        }
    }

    /// Clock 28: the flags the write-back obeys.
    pub(super) fn latch_echo_flags(&mut self) {
        self.echo_write_flags = self.regs[FLG];
    }

    /// Clock 29: latch the page, take the ring's length when it starts
    /// over, step one frame on, write the left half back, latch the
    /// flags again.
    pub(super) fn echo_advance(&mut self, aram: &mut [u8]) {
        self.echo_page = self.regs[ESA];
        if self.echo_offset == 0 {
            self.echo_length = u16::from(self.regs[EDL] & 0x0f) << 11;
        }
        self.echo_offset += 4;
        if self.echo_offset >= self.echo_length {
            self.echo_offset = 0;
        }
        self.write_back(0, aram);
        self.echo_write_flags = self.regs[FLG];
    }

    /// Clock 30: the right half written back.
    pub(super) fn echo_write_right(&mut self, aram: &mut [u8]) {
        self.write_back(1, aram);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aram() -> Vec<u8> {
        vec![0u8; 0x10000]
    }

    #[test]
    fn a_tap_scales_the_history_by_its_coefficient() {
        let mut dsp = SDsp::new();
        dsp.history_head = 0;
        dsp.echo_history[0][1] = 100;
        dsp.regs[FIR] = 64;
        assert_eq!(dsp.tap(0, 0), 100);
        dsp.history_head = 3;
        dsp.echo_history[1][6] = 256;
        dsp.regs[FIR + 2 * 0x10] = 0x80;
        assert_eq!(dsp.tap(1, 2), -512);
        // Tap 7 is the slot at the head: the frame just read.
        dsp.echo_history[1][3] = 10;
        dsp.regs[FIR + 7 * 0x10] = 64;
        assert_eq!(dsp.tap(1, 7), 10);
    }

    #[test]
    fn the_filter_cuts_the_first_seven_taps_to_16_bits() {
        let mut dsp = SDsp::new();
        dsp.history_head = 0;
        for slot in 0..8 {
            dsp.echo_history[0][slot] = 0x4000;
        }
        for tap in 0..8 {
            dsp.regs[FIR + tap * 0x10] = 0x7f;
        }
        dsp.echo_filter();
        // Each tap is (0x4000 * 127) >> 6 = 32512; seven of them wrap in
        // 16 bits to -34816 + 65536*k, then the eighth adds, then clamp.
        let tap = (0x4000 * 127) >> 6;
        let seven = i32::from((tap * 7) as i16);
        assert_eq!(dsp.echo_return[0], clamp16(seven + tap) & !1);
    }

    #[test]
    fn the_mix_adds_the_echo_through_its_volume() {
        let mut dsp = SDsp::new();
        dsp.main_sum[0] = 1000;
        dsp.echo_return[0] = 1000;
        dsp.regs[MVOLL] = 127;
        dsp.regs[EVOLL] = 0;
        let dry = dsp.mixed(0);
        dsp.regs[EVOLL] = 127;
        assert_eq!(dry, (1000 * 127) >> 7);
        assert_eq!(dsp.mixed(0), 2 * dry);
    }

    #[test]
    fn a_frame_read_in_is_halved_and_a_frame_written_back_is_the_sum() {
        let mut dsp = SDsp::new();
        let mut ram = aram();
        dsp.echo_pointer = 0x100;
        dsp.history_head = 0;
        dsp.echo_sum[0] = 0x1234;
        dsp.write_back(0, &mut ram);
        assert_eq!(&ram[0x100..0x102], &[0x34, 0x12]);
        assert_eq!(dsp.echo_sum[0], 0, "the sum starts over");
        dsp.read_into_history(0, &ram);
        assert_eq!(dsp.echo_history[0][0], 0x1234 >> 1);
        // The right half sits two bytes on, and the address wraps.
        dsp.echo_pointer = 0xfffe;
        dsp.echo_sum[1] = -2;
        dsp.write_back(1, &mut ram);
        assert_eq!(ram[0x0000], 0xfe);
        assert_eq!(ram[0x0001], 0xff);
    }

    #[test]
    fn the_write_back_obeys_the_latched_flags() {
        let mut dsp = SDsp::new();
        let mut ram = aram();
        dsp.echo_pointer = 0x200;
        dsp.echo_write_flags = FLG_ECHO_OFF;
        dsp.echo_sum[0] = 0x1234;
        dsp.write_back(0, &mut ram);
        assert_eq!(&ram[0x200..0x202], &[0, 0]);
        assert_eq!(dsp.echo_sum[0], 0);
    }

    #[test]
    fn the_ring_takes_its_length_when_it_starts_over() {
        let mut dsp = SDsp::new();
        let mut ram = aram();
        dsp.regs[EDL] = 2;
        dsp.echo_advance(&mut ram);
        assert_eq!(dsp.echo_length, 2 << 11);
        assert_eq!(dsp.echo_offset, 4);
        dsp.echo_offset = 2044;
        dsp.echo_length = 2048;
        dsp.regs[EDL] = 7; // not read until the ring wraps
        dsp.echo_advance(&mut ram);
        assert_eq!(dsp.echo_offset, 0);
        assert_eq!(dsp.echo_length, 2048);
        // EDL 0 is a ring of one frame.
        dsp.regs[EDL] = 0;
        dsp.echo_advance(&mut ram);
        assert_eq!(dsp.echo_length, 0);
        assert_eq!(dsp.echo_offset, 0);
    }

    #[test]
    fn the_feedback_adds_the_filtered_echo_to_the_sums() {
        let mut dsp = SDsp::new();
        dsp.echo_return = [1000, 0];
        dsp.regs[EFB] = 127;
        dsp.echo_feedback();
        assert_eq!(dsp.echo_sum, [992, 0]);
    }

    #[test]
    fn the_output_is_silent_while_flg_mutes() {
        let mut dsp = SDsp::new();
        dsp.main_sum = [1000, 1000];
        dsp.regs[MVOLL] = 127;
        dsp.regs[MVOLR] = 127;
        dsp.regs[FLG] = FLG_MUTE;
        assert_eq!(dsp.output(), (0, 0));
        assert_eq!(dsp.main_sum, [0, 0], "the sums are cleared either way");
        dsp.main_sum = [1000, 1000];
        dsp.regs[FLG] = 0;
        dsp.echo_feedback();
        assert_eq!(dsp.output(), (992, 992));
    }
}
