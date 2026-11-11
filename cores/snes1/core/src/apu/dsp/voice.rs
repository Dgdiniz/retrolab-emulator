//! The eight voices, and the stages of a voice's pass through the
//! datapath.
//!
//! A voice plays one BRR sample from ARAM. It reads the sample's start
//! and loop addresses from the directory, decodes blocks into its ring
//! four values at a time as its position advances by the pitch,
//! interpolates the sample at that position (or takes the noise
//! generator's value instead), scales it by its envelope, and adds the
//! result to the left and right sums through its two volumes, and,
//! when routed to the echo, to the echo sums too. A key-on restarts the
//! voice over five samples during which it is silent; a key-off, a soft
//! reset, or an end block without loop sends the envelope into release.
//!
//! The datapath keeps a few values from one clock to the next (the
//! directory entry, the block header and byte, the pitch, the output),
//! and because voices overlap in the timetable some are read by the next
//! voice: pitch modulation bends a voice's pitch by the previous voice's
//! output. Those values live on [`SDsp`]; a voice's own state is here.
//!
//! Written from fullsnes's voice registers and Anomie's description of
//! key-on, pitch modulation and the ENDX/ENVX/OUTX write-back; checked
//! against bsnes-plus.

use super::brr::{decode_four, BlockHeader, SampleRing};
use super::envelope::{Envelope, Stage};
use super::gaussian::interpolate;
use super::registers::{VoiceReg, ENDX, FLG, FLG_RESET};
use super::{clamp16, SDsp};

/// A pitch bent by the previous voice's output.
#[inline]
fn modulated(pitch: i32, previous_output: i32) -> i32 {
    pitch + (((previous_output >> 5) * pitch) >> 10)
}

/// The noise generator's value as a sample.
#[inline]
fn noise_sample(lfsr: u16) -> i32 {
    i32::from((lfsr << 1) as i16)
}

/// One voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Voice {
    /// The decoded samples the interpolator reads.
    pub ring: SampleRing,
    /// Where the voice is in its ring: twelve fraction bits, and above
    /// them the whole samples ahead of the ring's head.
    pub position: i32,
    /// ARAM address of the block being decoded.
    pub block_address: u16,
    /// The next data byte within that block (1, 3, 5 or 7).
    pub block_offset: u8,
    /// Samples left of the key-on set-up: 5 down to 0, 0 when playing.
    pub key_on_countdown: u8,
    pub envelope: Envelope,
    /// ENVX as computed this pass, written to the register a few clocks
    /// later.
    pub envx_out: u8,
}

impl Voice {
    pub(super) const SILENT: Voice = Voice {
        ring: SampleRing::EMPTY,
        position: 0,
        block_address: 0,
        block_offset: 1,
        key_on_countdown: 0,
        envelope: Envelope::SILENT,
        envx_out: 0,
    };

    /// One sample of the key-on set-up: on the first, start the sample
    /// over from `start`; on every one, hold the envelope and the
    /// position at zero and count down, except that on the three before
    /// the last the position is held four whole samples ahead (`0x4000`),
    /// so the ring fills with the sample's first values. Returns whether
    /// this was the first.
    fn key_on_setup(&mut self, start: u16) -> bool {
        let first = self.key_on_countdown == 5;
        if first {
            self.block_address = start;
            self.block_offset = 1;
            self.ring.rewind();
        }
        self.envelope.level = 0;
        self.envelope.unclamped = 0;
        self.position = 0;
        self.key_on_countdown -= 1;
        if self.key_on_countdown & 3 != 0 {
            self.position = 0x4000;
        }
        first
    }

    /// What this sample's events do to the envelope, in the chip's
    /// order: a cut (soft reset or an ending block) silences it, a key-off
    /// releases it, a key-on starts the set-up and the attack.
    fn key_events(&mut self, cut: bool, key_off: bool, key_on: bool) {
        if cut {
            self.envelope.cut();
        }
        if key_off {
            self.envelope.stage = Stage::Release;
        }
        if key_on {
            self.key_on_countdown = 5;
            self.envelope.stage = Stage::Attack;
        }
    }

    /// Decode the next four values of the block, `first` being the data
    /// byte fetched earlier and its successor read here.
    fn decode(&mut self, header: BlockHeader, first: u8, aram: &[u8]) {
        let second = (u32::from(self.block_address) + u32::from(self.block_offset) + 1) & 0xffff;
        decode_four(&mut self.ring, header, [first, aram[second as usize]]);
        self.block_offset += 2;
    }

    /// Every data byte of the block has been decoded.
    fn block_spent(&self) -> bool {
        self.block_offset >= 9
    }

    /// On to the block after this one, or, when this one ends the
    /// sample, to `loop_address`. Returns whether the sample ended.
    fn next_block(&mut self, header: BlockHeader, loop_address: u16) -> bool {
        self.block_address = self.block_address.wrapping_add(9);
        if header.ends {
            self.block_address = loop_address;
        }
        self.block_offset = 1;
        header.ends
    }

    /// Move on by the pitch; the whole part counts the samples passed but
    /// not yet replaced by a decode (`& 0x3fff` drops the four a decode
    /// has just replaced), and is capped at seven whole samples plus the
    /// fraction (`0x7fff`) so the ring is never outrun.
    fn advance(&mut self, pitch: i32) {
        self.position = ((self.position & 0x3fff) + pitch).min(0x7fff);
    }
}

impl SDsp {
    /// Latch the voice's source number and, from the number latched for
    /// the voice before it, form that voice's directory entry: the
    /// datapath carries one number while it reads the next.
    pub(super) fn latch_source(&mut self, voice: usize) {
        self.directory_entry =
            (u32::from(self.directory_page) << 8) + (u32::from(self.source_number) << 2);
        self.source_number = self.vreg(voice, VoiceReg::Srcn);
    }

    /// Read the sample's start address from its directory entry, its
    /// loop address once the voice is playing, and latch the envelope
    /// control byte and the low byte of the pitch.
    pub(super) fn read_directory(&mut self, voice: usize, aram: &[u8]) {
        let mut entry = self.directory_entry;
        if self.voices[voice].key_on_countdown == 0 {
            entry += 2;
        }
        let lo = aram[(entry & 0xffff) as usize];
        let hi = aram[((entry + 1) & 0xffff) as usize];
        self.next_block = u16::from_le_bytes([lo, hi]);
        self.envelope_control = self.vreg(voice, VoiceReg::Adsr1);
        self.pitch = i32::from(self.vreg(voice, VoiceReg::PitchL));
    }

    /// The high six bits of the pitch.
    pub(super) fn pitch_high(&mut self, voice: usize) {
        self.pitch += i32::from(self.vreg(voice, VoiceReg::PitchH) & 0x3f) << 8;
    }

    /// Read the current block's header and its next data byte.
    pub(super) fn fetch_block(&mut self, voice: usize, aram: &[u8]) {
        let v = &self.voices[voice];
        let next = (u32::from(v.block_address) + u32::from(v.block_offset)) & 0xffff;
        self.block_byte = aram[next as usize];
        self.block_header = aram[usize::from(v.block_address)];
    }

    /// Produce the voice's output for this sample: bend the pitch by the
    /// previous voice's output when modulated, run the key-on set-up,
    /// interpolate (or take the noise), apply the envelope, then honour a
    /// soft reset, an ending block, and this sample's key-off and key-on,
    /// and finally step the envelope.
    pub(super) fn synthesize(&mut self, voice: usize) {
        let bit = 1u8 << voice;
        if self.pitch_mod_mask & bit != 0 {
            self.pitch = modulated(self.pitch, self.voice_output);
        }
        let adsr2 = self.vreg(voice, VoiceReg::Adsr2);
        let gain = self.vreg(voice, VoiceReg::Gain);
        let soft_reset = self.regs[FLG] & FLG_RESET != 0;
        let next_block = self.next_block;

        let v = &mut self.voices[voice];
        if v.key_on_countdown != 0 {
            if v.key_on_setup(next_block) {
                // The header read this clock is ignored on the first
                // set-up sample.
                self.block_header = 0;
            }
            self.pitch = 0;
        }

        let sample = if self.noise_mask & bit != 0 {
            noise_sample(self.noise_lfsr)
        } else {
            interpolate(&v.ring, v.position)
        };
        self.voice_output = v.envelope.apply(sample);
        v.envx_out = v.envelope.envx();

        let ending = BlockHeader::decode(self.block_header).ends_without_loop();
        v.key_events(
            soft_reset || ending,
            self.key_sample && self.key_off_mask & bit != 0,
            self.key_sample && self.key_on_mask & bit != 0,
        );
        if v.key_on_countdown == 0 {
            v.envelope
                .step(self.envelope_control, adsr2, gain, self.rate_counter);
        }
    }

    /// Decode four more values when the position has passed the ones it
    /// had, moving to the next block, or back to the loop address at the
    /// end, when the block is spent; advance the position by the pitch;
    /// mix the left channel.
    pub(super) fn decode_mix_left(&mut self, voice: usize, aram: &[u8]) {
        let header = BlockHeader::decode(self.block_header);
        let v = &mut self.voices[voice];
        self.looped_mask = 0;
        if v.position >= 0x4000 {
            v.decode(header, self.block_byte, aram);
            if v.block_spent() && v.next_block(header, self.next_block) {
                self.looped_mask = 1 << voice;
            }
        }
        v.advance(self.pitch);
        self.mix(voice, 0);
    }

    /// Mix the right channel; shadow ENDX with this voice's end flag, or
    /// without it on the first key-on sample, which clears it.
    pub(super) fn mix_right(&mut self, voice: usize) {
        self.mix(voice, 1);
        self.endx_shadow = self.regs[ENDX] | self.looped_mask;
        if self.voices[voice].key_on_countdown == 5 {
            self.endx_shadow &= !(1 << voice);
        }
    }

    pub(super) fn shadow_outx(&mut self) {
        self.outx_shadow = self.voice_output >> 8;
    }

    pub(super) fn write_endx(&mut self, voice: usize) {
        self.regs[ENDX] = self.endx_shadow;
        self.envx_shadow = self.voices[voice].envx_out;
    }

    pub(super) fn write_outx(&mut self, voice: usize) {
        self.set_vreg(voice, VoiceReg::Outx, self.outx_shadow as u8);
    }

    pub(super) fn write_envx(&mut self, voice: usize) {
        self.set_vreg(voice, VoiceReg::Envx, self.envx_shadow);
    }

    /// Add the voice's output, through its volume for `channel`, to the
    /// main sum and, when the voice is routed to the echo, to the echo
    /// sum. A voice muted from the debugger still plays (its registers
    /// keep reading back right) but adds nothing.
    fn mix(&mut self, voice: usize, channel: usize) {
        if self.voice_muted(voice) {
            return;
        }
        let volume = if channel == 0 {
            VoiceReg::VolL
        } else {
            VoiceReg::VolR
        };
        let volume = i32::from(self.vreg(voice, volume) as i8);
        let amount = (self.voice_output * volume) >> 7;
        self.main_sum[channel] = clamp16(self.main_sum[channel] + amount);
        if self.echo_mask & (1 << voice) != 0 {
            self.echo_sum[channel] = clamp16(self.echo_sum[channel] + amount);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::registers::{EON, PMON};
    use super::*;

    #[test]
    fn mixing_adds_through_the_volume_and_follows_the_echo_routing() {
        let mut dsp = SDsp::new();
        dsp.voice_output = 1000;
        dsp.set_vreg(2, VoiceReg::VolL, 127);
        dsp.set_vreg(2, VoiceReg::VolR, 0x80);
        dsp.echo_mask = 0;
        dsp.mix(2, 0);
        assert_eq!(dsp.main_sum[0], (1000 * 127) >> 7);
        assert_eq!(dsp.echo_sum[0], 0, "not routed to the echo");
        dsp.echo_mask = 1 << 2;
        dsp.mix(2, 1);
        assert_eq!(dsp.main_sum[1], (1000 * -128) >> 7);
        assert_eq!(dsp.echo_sum[1], (1000 * -128) >> 7);
    }

    #[test]
    fn a_muted_voice_adds_nothing_until_unmuted() {
        let mut dsp = SDsp::new();
        dsp.voice_output = 1000;
        dsp.set_vreg(0, VoiceReg::VolL, 127);
        dsp.set_voice_mute(0, true);
        dsp.mix(0, 0);
        assert_eq!(dsp.main_sum[0], 0);
        dsp.set_voice_mute(0, false);
        dsp.mix(0, 0);
        assert_eq!(dsp.main_sum[0], (1000 * 127) >> 7);
    }

    #[test]
    fn the_source_number_is_carried_one_voice_behind() {
        let mut dsp = SDsp::new();
        dsp.regs[super::super::registers::DIR] = 0x02;
        dsp.latch_routing();
        dsp.set_vreg(3, VoiceReg::Srcn, 0x10);
        dsp.set_vreg(4, VoiceReg::Srcn, 0x20);
        dsp.latch_source(3);
        dsp.latch_source(4);
        // Voice 4's step formed voice 3's entry: page 2, number 0x10.
        assert_eq!(dsp.directory_entry, 0x0200 + (0x10 << 2));
        assert_eq!(dsp.source_number, 0x20);
    }

    #[test]
    fn the_directory_gives_the_start_then_the_loop_address() {
        let mut dsp = SDsp::new();
        let mut aram = vec![0u8; 0x10000];
        aram[0x0240..0x0244].copy_from_slice(&[0x00, 0x30, 0x09, 0x30]);
        dsp.directory_entry = 0x0240;
        dsp.voices[1].key_on_countdown = 5;
        dsp.read_directory(1, &aram);
        assert_eq!(dsp.next_block, 0x3000);
        dsp.voices[1].key_on_countdown = 0;
        dsp.read_directory(1, &aram);
        assert_eq!(dsp.next_block, 0x3009);
    }

    #[test]
    fn a_key_on_restarts_the_voice_and_silences_the_set_up() {
        let mut dsp = SDsp::new();
        dsp.next_block = 0x4000;
        dsp.voices[0].key_on_countdown = 5;
        dsp.voices[0].envelope.level = 0x300;
        dsp.voices[0].position = 0x1234;
        dsp.pitch = 0x1000;
        dsp.synthesize(0);
        let v = &dsp.voices[0];
        assert_eq!(v.block_address, 0x4000);
        assert_eq!(v.block_offset, 1);
        assert_eq!(v.envelope.level, 0);
        assert_eq!(v.key_on_countdown, 4);
        assert_eq!(dsp.pitch, 0, "no pitch during the set-up");
        assert_eq!(dsp.voice_output, 0);
    }

    #[test]
    fn a_key_on_this_sample_starts_the_countdown_and_the_attack() {
        let mut dsp = SDsp::new();
        dsp.key_sample = true;
        dsp.key_on_mask = 1 << 3;
        dsp.synthesize(3);
        assert_eq!(dsp.voices[3].key_on_countdown, 5);
        assert_eq!(dsp.voices[3].envelope.stage, Stage::Attack);
        // Not on a sample that does not honour the keys.
        dsp.key_sample = false;
        dsp.key_on_mask = 1 << 4;
        dsp.synthesize(4);
        assert_eq!(dsp.voices[4].key_on_countdown, 0);
    }

    #[test]
    fn an_end_block_without_loop_cuts_the_voice() {
        let mut dsp = SDsp::new();
        dsp.voices[5].envelope = Envelope {
            stage: Stage::Sustain,
            level: 0x400,
            unclamped: 0x400,
        };
        dsp.block_header = 0x01;
        dsp.synthesize(5);
        assert_eq!(dsp.voices[5].envelope.stage, Stage::Release);
        assert_eq!(dsp.voices[5].envelope.level, 0);
        // The output this sample still used the level it had.
        assert_eq!(dsp.voices[5].envx_out, 0x40);
    }

    #[test]
    fn pitch_modulation_bends_the_pitch_by_the_previous_output() {
        let mut dsp = SDsp::new();
        dsp.regs[PMON] = 1 << 1;
        dsp.latch_pitch_mod();
        dsp.voice_output = 0x4000;
        dsp.pitch = 0x1000;
        dsp.synthesize(1);
        // 0x1000 + ((0x4000 >> 5) * 0x1000) >> 10 = 0x1000 + 0x800.
        assert_eq!(dsp.pitch, 0x1800);
    }

    #[test]
    fn the_position_advances_by_the_pitch_and_decodes_when_due() {
        let mut dsp = SDsp::new();
        let aram = vec![0u8; 0x10000];
        dsp.pitch = 0x1000;
        dsp.voices[0].position = 0x3fff;
        dsp.decode_mix_left(0, &aram);
        assert_eq!(dsp.voices[0].position, 0x4fff);
        assert_eq!(dsp.voices[0].block_offset, 1, "nothing decoded yet");
        dsp.decode_mix_left(0, &aram);
        assert_eq!(dsp.voices[0].block_offset, 3, "four values decoded");
        assert_eq!(dsp.voices[0].position, 0x1fff);
    }

    #[test]
    fn the_end_of_an_ending_block_loops_and_flags_endx() {
        let mut dsp = SDsp::new();
        let aram = vec![0u8; 0x10000];
        dsp.block_header = 0x03;
        dsp.next_block = 0x5000;
        dsp.voices[6].block_address = 0x1000;
        dsp.voices[6].block_offset = 7;
        dsp.voices[6].position = 0x4000;
        dsp.decode_mix_left(6, &aram);
        assert_eq!(dsp.voices[6].block_address, 0x5000);
        assert_eq!(dsp.voices[6].block_offset, 1);
        assert_eq!(dsp.looped_mask, 1 << 6);
        dsp.mix_right(6);
        assert_eq!(dsp.endx_shadow, 0xff | (1 << 6));
    }

    #[test]
    fn the_shadows_reach_the_registers_a_few_clocks_later() {
        let mut dsp = SDsp::new();
        dsp.regs[ENDX] = 0;
        dsp.looped_mask = 1 << 2;
        dsp.voice_output = -0x1200;
        dsp.voices[2].envx_out = 0x55;
        dsp.mix_right(2);
        dsp.shadow_outx();
        dsp.write_endx(2);
        dsp.write_outx(2);
        dsp.write_envx(2);
        assert_eq!(dsp.regs[ENDX], 1 << 2);
        assert_eq!(dsp.vreg(2, VoiceReg::Outx), (-0x12i32) as u8);
        assert_eq!(dsp.vreg(2, VoiceReg::Envx), 0x55);
        // EON is read through its latch, not the register.
        dsp.regs[EON] = 0xff;
        assert_eq!(dsp.echo_mask, 0);
    }
}
