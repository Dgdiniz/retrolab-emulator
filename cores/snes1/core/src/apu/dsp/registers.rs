//! The register file: 128 bytes the SPC700 reaches through the address
//! port at `$F2` and the data port at `$F3`. Addresses `$80-$FF` read the
//! same bytes again and ignore writes.
//!
//! The file is a grid: the low nibble of an address is the column, the
//! high nibble the row. Columns 0-9 belong to the voices, one row each,
//! volume left and right, pitch low and high, source number, the two
//! ADSR bytes, GAIN, and the read-back ENVX and OUTX. Columns `$C`, `$D`
//! and `$F` hold the global registers down their rows: the main and echo
//! volumes, KON, KOFF, FLG and ENDX in `$C`; the echo feedback, the
//! pitch-modulation, noise and echo enables, the directory and echo
//! pages and the echo delay in `$D`; the eight filter coefficients in
//! `$F`.
//!
//! A write lands in the file at once. Four of them do more: KON is also
//! held for the next sample that honours it, a write to ENDX clears it
//! whatever the value, and a write to ENVX or OUTX also goes into the
//! shadow the chip writes those registers from, so it is not undone by
//! the write-back a few clocks later.
//!
//! Written from fullsnes's DSP register map; checked against bsnes-plus.

use super::SDsp;

/// How many registers there are; the upper 128 addresses mirror them.
pub(in crate::apu) const DSP_REG_COUNT: usize = 128;

// Column `$C`.
pub(super) const MVOLL: usize = 0x0c;
pub(super) const MVOLR: usize = 0x1c;
pub(super) const EVOLL: usize = 0x2c;
pub(super) const EVOLR: usize = 0x3c;
pub(super) const KON: usize = 0x4c;
pub(super) const KOFF: usize = 0x5c;
pub(super) const FLG: usize = 0x6c;
pub(super) const ENDX: usize = 0x7c;
// Column `$D`.
pub(super) const EFB: usize = 0x0d;
pub(super) const PMON: usize = 0x2d;
pub(super) const NON: usize = 0x3d;
pub(super) const EON: usize = 0x4d;
pub(super) const DIR: usize = 0x5d;
pub(super) const ESA: usize = 0x6d;
pub(super) const EDL: usize = 0x7d;
/// Column `$F`: coefficient `n` is at `FIR + n * 0x10`.
pub(super) const FIR: usize = 0x0f;

/// FLG bit 7: soft reset, every voice silenced and held in release.
pub(super) const FLG_RESET: u8 = 0x80;
/// FLG bit 6: the output is muted.
pub(super) const FLG_MUTE: u8 = 0x40;
/// FLG bit 5: the echo ring is read but not written.
pub(super) const FLG_ECHO_OFF: u8 = 0x20;

/// A voice's registers: the column within its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VoiceReg {
    /// Left volume, signed.
    VolL = 0,
    /// Right volume, signed.
    VolR = 1,
    /// Pitch, low byte.
    PitchL = 2,
    /// Pitch, high byte (six bits).
    PitchH = 3,
    /// Source number: which directory entry the voice plays.
    Srcn = 4,
    /// Bit 7 selects ADSR over GAIN; bits 4-6 the decay rate; bits 0-3
    /// the attack rate.
    Adsr1 = 5,
    /// Bits 5-7 the sustain level; bits 0-4 the sustain rate.
    Adsr2 = 6,
    /// A level, or one of four slopes, when ADSR is off.
    Gain = 7,
    /// The envelope, read back.
    Envx = 8,
    /// The output, read back.
    Outx = 9,
}

/// A voice as the audio debugger sees it: its registers and the live
/// envelope, decoded.
#[derive(Debug, Clone, Copy)]
pub(in crate::apu) struct VoiceDebug {
    pub voll: i8,
    pub volr: i8,
    /// The 14-bit pitch.
    pub pitch: u16,
    pub adsr0: u8,
    pub adsr1: u8,
    pub gain: u8,
    /// OUTX, signed.
    pub outx: i8,
    /// The envelope's level, 0..=2047.
    pub env_level: i32,
    /// The envelope's stage: 0 release, 1 attack, 2 decay, 3 sustain.
    pub env_mode: u8,
    /// ADSR selected rather than GAIN.
    pub adsr_enabled: bool,
    pub key_on: bool,
    pub ended: bool,
    pub noise: bool,
    /// Muted from the debugger.
    pub muted: bool,
}

impl SDsp {
    #[inline]
    pub(super) fn vreg(&self, voice: usize, reg: VoiceReg) -> u8 {
        self.regs[voice * 0x10 + reg as usize]
    }

    #[inline]
    pub(super) fn set_vreg(&mut self, voice: usize, reg: VoiceReg, value: u8) {
        self.regs[voice * 0x10 + reg as usize] = value;
    }

    /// The address port, `$F2`.
    pub(in crate::apu) fn addr(&self) -> u8 {
        self.addr
    }

    pub(in crate::apu) fn set_addr(&mut self, value: u8) {
        self.addr = value;
    }

    /// The data port, `$F3`, read.
    pub(in crate::apu) fn read_data(&self) -> u8 {
        self.regs[usize::from(self.addr & 0x7f)]
    }

    /// The data port, `$F3`, written.
    pub(in crate::apu) fn write_data(&mut self, value: u8) {
        if self.addr & 0x80 != 0 {
            return;
        }
        let at = usize::from(self.addr & 0x7f);
        self.regs[at] = value;
        match at & 0x0f {
            8 => self.envx_shadow = value,
            9 => self.outx_shadow = i32::from(value),
            0xc if at == KON => self.key_on_pending = value,
            0xc if at == ENDX => {
                self.endx_shadow = 0;
                self.regs[ENDX] = 0;
            }
            _ => {}
        }
    }

    /// The whole file, as the debugger shows it.
    pub(in crate::apu) fn regs(&self) -> &[u8; DSP_REG_COUNT] {
        &self.regs
    }

    pub(in crate::apu) fn voice_debug(&self, voice: usize) -> VoiceDebug {
        let adsr0 = self.vreg(voice, VoiceReg::Adsr1);
        let pitch = u16::from_le_bytes([
            self.vreg(voice, VoiceReg::PitchL),
            self.vreg(voice, VoiceReg::PitchH),
        ]) & 0x3fff;
        let bit = 1u8 << voice;
        let envelope = &self.voices[voice].envelope;
        VoiceDebug {
            voll: self.vreg(voice, VoiceReg::VolL) as i8,
            volr: self.vreg(voice, VoiceReg::VolR) as i8,
            pitch,
            adsr0,
            adsr1: self.vreg(voice, VoiceReg::Adsr2),
            gain: self.vreg(voice, VoiceReg::Gain),
            outx: self.vreg(voice, VoiceReg::Outx) as i8,
            env_level: i32::from(envelope.level),
            env_mode: envelope.stage.code(),
            adsr_enabled: adsr0 & 0x80 != 0,
            key_on: self.regs[KON] & bit != 0,
            ended: self.regs[ENDX] & bit != 0,
            noise: self.regs[NON] & bit != 0,
            muted: self.voice_mute[voice],
        }
    }

    /// Mute or unmute a voice from the debugger; an index past 7 does
    /// nothing.
    pub(in crate::apu) fn set_voice_mute(&mut self, voice: usize, muted: bool) {
        if voice < 8 {
            self.voice_mute[voice] = muted;
        }
    }

    pub(in crate::apu) fn voice_muted(&self, voice: usize) -> bool {
        voice < 8 && self.voice_mute[voice]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chip_powers_up_muted_in_reset_with_every_voice_ended() {
        let dsp = SDsp::new();
        assert_eq!(dsp.regs()[FLG], FLG_RESET | FLG_MUTE | FLG_ECHO_OFF);
        assert_eq!(dsp.regs()[ENDX], 0xff);
        assert_eq!(dsp.addr(), 0);
    }

    #[test]
    fn the_data_port_reads_and_writes_the_selected_register() {
        let mut dsp = SDsp::new();
        dsp.set_addr(0x12);
        dsp.write_data(0xab);
        assert_eq!(dsp.read_data(), 0xab);
        assert_eq!(dsp.vreg(1, VoiceReg::PitchL), 0xab);
    }

    #[test]
    fn the_upper_half_mirrors_the_file_read_only() {
        let mut dsp = SDsp::new();
        dsp.set_addr(0x05);
        dsp.write_data(0x33);
        dsp.set_addr(0x85);
        dsp.write_data(0x99);
        assert_eq!(dsp.read_data(), 0x33);
    }

    #[test]
    fn a_kon_write_waits_for_a_key_sample() {
        let mut dsp = SDsp::new();
        dsp.set_addr(KON as u8);
        dsp.write_data(0b0000_0101);
        assert_eq!(dsp.key_on_pending, 0b0000_0101);
        assert_eq!(dsp.regs()[KON], 0b0000_0101);
    }

    #[test]
    fn any_write_to_endx_clears_it() {
        let mut dsp = SDsp::new();
        dsp.endx_shadow = 0x55;
        dsp.set_addr(ENDX as u8);
        dsp.write_data(0xff);
        assert_eq!(dsp.read_data(), 0);
        assert_eq!(dsp.endx_shadow, 0);
    }

    #[test]
    fn envx_and_outx_writes_also_set_the_shadows() {
        let mut dsp = SDsp::new();
        dsp.set_addr(0x38); // voice 3 ENVX
        dsp.write_data(0x7e);
        assert_eq!(dsp.envx_shadow, 0x7e);
        dsp.set_addr(0x69); // voice 6 OUTX
        dsp.write_data(0xf0);
        assert_eq!(dsp.outx_shadow, 0xf0);
    }

    #[test]
    fn the_debug_view_decodes_a_voice() {
        let mut dsp = SDsp::new();
        dsp.set_vreg(2, VoiceReg::VolL, 0x90);
        dsp.set_vreg(2, VoiceReg::PitchL, 0x34);
        dsp.set_vreg(2, VoiceReg::PitchH, 0xd2);
        dsp.set_vreg(2, VoiceReg::Adsr1, 0x8f);
        dsp.regs[KON] = 1 << 2;
        dsp.regs[NON] = 1 << 2;
        dsp.regs[ENDX] = 0;
        dsp.voices[2].envelope.level = 0x123;
        dsp.set_voice_mute(2, true);
        let vd = dsp.voice_debug(2);
        assert_eq!(vd.voll, -0x70);
        assert_eq!(vd.pitch, 0x1234);
        assert!(vd.adsr_enabled);
        assert_eq!(vd.env_level, 0x123);
        assert_eq!(vd.env_mode, 0);
        assert!(vd.key_on && vd.noise && !vd.ended && vd.muted);
        assert!(!dsp.voice_muted(8));
    }
}
