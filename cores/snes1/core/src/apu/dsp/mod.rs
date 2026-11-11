//! The S-DSP, the SNES sound chip: eight voices playing BRR samples out
//! of the 64 KiB of audio RAM, an echo unit with an eight-tap filter, a
//! noise generator, and a stereo mix, one 16-bit sample pair every 32
//! clocks of the 1.024 MHz clock, 32 kHz.
//!
//! The chip has one datapath, and it serves the voices in turn: each
//! sample it runs a timetable of 32 clocks ([`schedule`]) in which every
//! voice goes through the same pass (latch the source, read the
//! directory, fetch the block, synthesize, decode and mix, write the
//! read-back registers), three clocks behind the voice before it, with
//! the echo unit, the register latches and the output in the last ten
//! clocks. Values the datapath carries from one clock to the next are
//! fields of [`SDsp`]; some are read by the next voice in line (pitch
//! modulation takes the previous voice's output; the directory entry is
//! formed one voice behind the source number). Registers the chip reads
//! once per sample (the modulation, noise and echo enables, the
//! directory page, KON and KOFF) are latched into their own fields at
//! their clock, and the read-back registers ENDX, ENVX and OUTX are
//! written from shadows a few clocks after they are computed, so a
//! write from the SPC700 in between is kept.
//!
//! The SPC700 and the DSP are not interleaved within a sample: the APU
//! runs the CPU and emits one whole sample every 32 of its cycles
//! ([`crate::apu::Apu::catch_up_to`]), so a register write always lands
//! between two samples. The timetable still has to be the chip's, clock
//! by clock, because the datapath's carried values make the result
//! depend on the order within a sample, and because voice 0's pass
//! straddles the sample boundary.
//!
//! Written from fullsnes's SNES APU chapter and Anomie's SPC700/DSP
//! notes (the register map, the BRR format, the ADSR/GAIN rules and rate
//! table, the echo unit, the key-on sequence and the per-clock
//! timetable); the output was checked, sample for sample, against
//! bsnes-plus through the golden runs.

mod brr;
mod echo;
mod envelope;
mod gaussian;
mod registers;
mod schedule;
mod voice;

use echo::HISTORY_LEN;
use envelope::{count_down, rate_fires, Stage};
use registers::{DIR, ENDX, EON, FLG, KOFF, NON, PMON};
use schedule::{Step, VoiceStage};
use voice::Voice;

pub(super) use registers::{VoiceDebug, DSP_REG_COUNT};

/// The DAC's range.
#[inline]
fn clamp16(value: i32) -> i32 {
    value.clamp(-0x8000, 0x7fff)
}

/// The envelope's range.
#[inline]
fn clamp11(value: i32) -> i32 {
    value.clamp(0, 0x7ff)
}

/// The noise generator: a 15-bit shift register fed from its two lowest
/// bits.
#[inline]
fn next_noise(lfsr: u16) -> u16 {
    let bit = (lfsr ^ (lfsr >> 1)) & 1;
    (lfsr >> 1) ^ (bit << 14)
}

/// The chip.
#[derive(Debug, Clone)]
pub(super) struct SDsp {
    regs: [u8; DSP_REG_COUNT],
    /// The address port, `$F2`.
    addr: u8,
    voices: [Voice; 8],
    /// Voices muted from the debugger: not chip state, not saved. A muted
    /// voice runs as before and adds nothing to the mix.
    voice_mute: [bool; 8],

    // The key latches. KON and KOFF are honoured on every other sample.
    /// This sample honours them.
    key_sample: bool,
    /// KON as latched on the last key sample.
    key_on_mask: u8,
    /// KON as written, waiting for a key sample.
    key_on_pending: u8,
    /// KOFF as latched on the last key sample.
    key_off_mask: u8,

    // The generators every voice shares.
    noise_lfsr: u16,
    /// The rate counter, counting down over 30720 samples.
    rate_counter: u16,

    // The echo ring and its history.
    /// The last eight frames read from the ring, halved, per channel.
    echo_history: [[i16; HISTORY_LEN]; 2],
    /// The slot the newest frame is in.
    history_head: u8,
    /// Bytes into the ring.
    echo_offset: u16,
    /// Bytes in the ring, taken from EDL each time the offset wraps.
    echo_length: u16,

    // The read-back registers, written a few clocks after they are
    // computed; a write from the SPC700 in between lands here too.
    endx_shadow: u8,
    envx_shadow: u8,
    outx_shadow: i32,

    // Latched once per sample.
    /// PMON, less voice 0 (it has no previous voice to follow).
    pitch_mod_mask: u8,
    noise_mask: u8,
    echo_mask: u8,
    directory_page: u8,

    // The datapath's registers, carried from clock to clock.
    /// Where the voice's sample starts, or loops.
    next_block: u16,
    /// The voice's ADSR1 byte, as latched with the directory read.
    envelope_control: u8,
    block_header: u8,
    block_byte: u8,
    /// The source number latched last; the next voice's step turns it
    /// into a directory entry.
    source_number: u8,
    /// ESA as latched.
    echo_page: u8,
    /// FLG as latched for the write-back.
    echo_write_flags: u8,
    /// Directory page x 256 + source number x 4.
    directory_entry: u32,
    pitch: i32,
    /// The last voice's output, after its envelope.
    voice_output: i32,
    /// The bit of a voice that reached an end block this clock.
    looped_mask: u8,
    /// ARAM address of this sample's frame in the echo ring.
    echo_pointer: u16,
    /// The voices' outputs for the two channels.
    main_sum: [i32; 2],
    /// What goes back into the echo ring.
    echo_sum: [i32; 2],
    /// What came out of the filter.
    echo_return: [i32; 2],
}

impl Default for SDsp {
    fn default() -> Self {
        Self::new()
    }
}

impl SDsp {
    /// The chip at power-on: everything zero but FLG, which holds it in
    /// soft reset, muted, with the echo write off, and ENDX, which flags
    /// every voice as ended. The audio driver clears FLG once its voices
    /// are set up.
    pub(super) fn new() -> Self {
        let mut regs = [0; DSP_REG_COUNT];
        regs[FLG] = 0xe0;
        regs[ENDX] = 0xff;
        Self {
            regs,
            addr: 0,
            voices: [Voice::SILENT; 8],
            voice_mute: [false; 8],
            key_sample: true,
            key_on_mask: 0,
            key_on_pending: 0,
            key_off_mask: 0,
            noise_lfsr: 0x4000,
            rate_counter: 0,
            echo_history: [[0; HISTORY_LEN]; 2],
            history_head: 0,
            echo_offset: 0,
            echo_length: 0,
            endx_shadow: 0,
            envx_shadow: 0,
            outx_shadow: 0,
            pitch_mod_mask: 0,
            noise_mask: 0,
            echo_mask: 0,
            directory_page: 0,
            next_block: 0,
            envelope_control: 0,
            block_header: 0,
            block_byte: 0,
            source_number: 0,
            echo_page: 0,
            echo_write_flags: 0,
            directory_entry: 0,
            pitch: 0,
            voice_output: 0,
            looped_mask: 0,
            echo_pointer: 0,
            main_sum: [0; 2],
            echo_sum: [0; 2],
            echo_return: [0; 2],
        }
    }

    /// Back to power-on; the debugger's mutes stay.
    pub(super) fn reset(&mut self) {
        let voice_mute = self.voice_mute;
        *self = Self::new();
        self.voice_mute = voice_mute;
    }

    /// One step of the timetable. `run_sample` (in `schedule`) calls
    /// this once per step with the step as a constant, so the match
    /// folds away.
    #[inline(always)]
    fn step(&mut self, step: Step, aram: &mut [u8], out: &mut (i16, i16)) {
        match step {
            Step::Voice(voice, stage) => self.voice_step(usize::from(voice), stage, aram),
            Step::EchoLeftIn => self.echo_left_in(aram),
            Step::EchoRightIn => self.echo_right_in(aram),
            Step::EchoFilter => self.echo_filter(),
            Step::EchoFeedback => self.echo_feedback(),
            Step::LatchPitchMod => self.latch_pitch_mod(),
            Step::Output => *out = self.output(),
            Step::LatchRouting => self.latch_routing(),
            Step::LatchEchoFlags => self.latch_echo_flags(),
            Step::ToggleKeySample => self.toggle_key_sample(),
            Step::EchoAdvance => self.echo_advance(aram),
            Step::LatchKeys => self.latch_keys(),
            Step::EchoWriteRight => self.echo_write_right(aram),
        }
    }

    #[inline(always)]
    fn voice_step(&mut self, voice: usize, stage: VoiceStage, aram: &[u8]) {
        match stage {
            VoiceStage::LatchSource => self.latch_source(voice),
            VoiceStage::ReadDirectory => self.read_directory(voice, aram),
            VoiceStage::PitchHigh => self.pitch_high(voice),
            VoiceStage::FetchBlock => self.fetch_block(voice, aram),
            VoiceStage::Synthesize => self.synthesize(voice),
            VoiceStage::Stage3 => {
                self.pitch_high(voice);
                self.fetch_block(voice, aram);
                self.synthesize(voice);
            }
            VoiceStage::DecodeMixLeft => self.decode_mix_left(voice, aram),
            VoiceStage::MixRight => self.mix_right(voice),
            VoiceStage::ShadowOutx => self.shadow_outx(),
            VoiceStage::WriteEndx => self.write_endx(voice),
            VoiceStage::WriteOutx => self.write_outx(voice),
            VoiceStage::WriteEnvx => self.write_envx(voice),
        }
    }

    /// Clock 27: PMON, less voice 0.
    fn latch_pitch_mod(&mut self) {
        self.pitch_mod_mask = self.regs[PMON] & !1;
    }

    /// Clock 28: NON, EON and the directory page.
    fn latch_routing(&mut self) {
        self.noise_mask = self.regs[NON];
        self.echo_mask = self.regs[EON];
        self.directory_page = self.regs[DIR];
    }

    /// Clock 29: alternate the key samples; on one, drop from the pending
    /// KON the bits the last key sample already took.
    fn toggle_key_sample(&mut self) {
        self.key_sample = !self.key_sample;
        if self.key_sample {
            self.key_on_pending &= !self.key_on_mask;
        }
    }

    /// Clock 30: on a key sample, take KON and KOFF; then count the rate
    /// counter down and, at the rate FLG names, clock the noise.
    fn latch_keys(&mut self) {
        if self.key_sample {
            self.key_on_mask = self.key_on_pending;
            self.key_off_mask = self.regs[KOFF];
        }
        self.rate_counter = count_down(self.rate_counter);
        if rate_fires(self.rate_counter, self.regs[FLG] & 0x1f) {
            self.noise_lfsr = next_noise(self.noise_lfsr);
        }
    }

    /// Everything a restore needs to carry on mid-stream, bit for bit.
    pub(super) fn to_savestate(&self) -> crate::savestate::DspState {
        use crate::savestate::{DspState, VoiceState};
        let voice = |v: &Voice| {
            let (samples, head) = v.ring.parts();
            VoiceState {
                samples: samples.map(i32::from),
                ring_head: i32::from(head),
                position: v.position,
                block_address: i32::from(v.block_address),
                block_offset: i32::from(v.block_offset),
                key_on_countdown: i32::from(v.key_on_countdown),
                stage: v.envelope.stage.code(),
                level: i32::from(v.envelope.level),
                envx_out: i32::from(v.envx_out),
                level_unclamped: v.envelope.unclamped,
            }
        };
        DspState {
            regs: self.regs.to_vec(),
            addr: self.addr,
            voices: core::array::from_fn(|i| voice(&self.voices[i])),
            key_sample: self.key_sample,
            key_on_mask: i32::from(self.key_on_mask),
            key_on_pending: i32::from(self.key_on_pending),
            noise_lfsr: i32::from(self.noise_lfsr),
            rate_counter: i32::from(self.rate_counter),
            echo_history: [
                self.echo_history[0].map(i32::from),
                self.echo_history[1].map(i32::from),
            ],
            history_head: i32::from(self.history_head),
            echo_offset: i32::from(self.echo_offset),
            echo_length: i32::from(self.echo_length),
            endx_shadow: i32::from(self.endx_shadow),
            envx_shadow: i32::from(self.envx_shadow),
            outx_shadow: self.outx_shadow,
            pitch_mod_mask: i32::from(self.pitch_mod_mask),
            noise_mask: i32::from(self.noise_mask),
            echo_mask: i32::from(self.echo_mask),
            directory_page: i32::from(self.directory_page),
            key_off_mask: i32::from(self.key_off_mask),
            next_block: i32::from(self.next_block),
            envelope_control: i32::from(self.envelope_control),
            block_header: i32::from(self.block_header),
            block_byte: i32::from(self.block_byte),
            source_number: i32::from(self.source_number),
            echo_page: i32::from(self.echo_page),
            echo_write_flags: i32::from(self.echo_write_flags),
            directory_entry: self.directory_entry as i32,
            pitch: self.pitch,
            voice_output: self.voice_output,
            looped_mask: i32::from(self.looped_mask),
            echo_pointer: i32::from(self.echo_pointer),
            main_sum: self.main_sum,
            echo_sum: self.echo_sum,
            echo_return: self.echo_return,
        }
    }

    /// The inverse of [`Self::to_savestate`]; `regs` is validated by the
    /// loader. The values a state holds were put there by this chip, so
    /// the narrowing is exact.
    #[allow(clippy::wrong_self_convention)]
    pub(super) fn from_savestate(&mut self, s: &crate::savestate::DspState) {
        self.regs.copy_from_slice(&s.regs);
        self.addr = s.addr;
        for (voice, saved) in self.voices.iter_mut().zip(s.voices.iter()) {
            voice.ring =
                brr::SampleRing::from_parts(saved.samples.map(|x| x as i16), saved.ring_head as u8);
            voice.position = saved.position;
            voice.block_address = saved.block_address as u16;
            voice.block_offset = saved.block_offset as u8;
            voice.key_on_countdown = saved.key_on_countdown as u8;
            voice.envelope.stage = Stage::from_code(saved.stage);
            voice.envelope.level = saved.level as u16;
            voice.envx_out = saved.envx_out as u8;
            voice.envelope.unclamped = saved.level_unclamped;
        }
        self.key_sample = s.key_sample;
        self.key_on_mask = s.key_on_mask as u8;
        self.key_on_pending = s.key_on_pending as u8;
        self.noise_lfsr = s.noise_lfsr as u16;
        self.rate_counter = s.rate_counter as u16;
        self.echo_history = [
            s.echo_history[0].map(|x| x as i16),
            s.echo_history[1].map(|x| x as i16),
        ];
        self.history_head = s.history_head as u8;
        self.echo_offset = s.echo_offset as u16;
        self.echo_length = s.echo_length as u16;
        self.endx_shadow = s.endx_shadow as u8;
        self.envx_shadow = s.envx_shadow as u8;
        self.outx_shadow = s.outx_shadow;
        self.pitch_mod_mask = s.pitch_mod_mask as u8;
        self.noise_mask = s.noise_mask as u8;
        self.echo_mask = s.echo_mask as u8;
        self.directory_page = s.directory_page as u8;
        self.key_off_mask = s.key_off_mask as u8;
        self.next_block = s.next_block as u16;
        self.envelope_control = s.envelope_control as u8;
        self.block_header = s.block_header as u8;
        self.block_byte = s.block_byte as u8;
        self.source_number = s.source_number as u8;
        self.echo_page = s.echo_page as u8;
        self.echo_write_flags = s.echo_write_flags as u8;
        self.directory_entry = s.directory_entry as u32;
        self.pitch = s.pitch;
        self.voice_output = s.voice_output;
        self.looped_mask = s.looped_mask as u8;
        self.echo_pointer = s.echo_pointer as u16;
        self.main_sum = s.main_sum;
        self.echo_sum = s.echo_sum;
        self.echo_return = s.echo_return;
    }
}

#[cfg(test)]
mod tests {
    use super::registers::{VoiceReg, KON};
    use super::*;

    #[test]
    fn kon_and_koff_are_honoured_every_other_sample() {
        let mut dsp = SDsp::new();
        dsp.key_on_pending = 0b0000_0001;
        dsp.regs[KOFF] = 0b0000_0010;
        dsp.key_sample = false;
        dsp.latch_keys();
        assert_eq!(dsp.key_on_mask, 0, "not on this sample");
        assert_eq!(dsp.key_off_mask, 0);
        dsp.toggle_key_sample();
        assert!(dsp.key_sample);
        dsp.latch_keys();
        assert_eq!(dsp.key_on_mask, 0b0000_0001);
        assert_eq!(dsp.key_off_mask, 0b0000_0010);
        // The next key sample drops what was taken from the pending KON.
        dsp.toggle_key_sample();
        dsp.toggle_key_sample();
        assert_eq!(dsp.key_on_pending, 0);
    }

    #[test]
    fn the_latches_take_their_registers() {
        let mut dsp = SDsp::new();
        dsp.regs[PMON] = 0b1010_0101;
        dsp.regs[NON] = 0x11;
        dsp.regs[EON] = 0x22;
        dsp.regs[DIR] = 0x33;
        dsp.latch_pitch_mod();
        dsp.latch_routing();
        assert_eq!(
            dsp.pitch_mod_mask, 0b1010_0100,
            "voice 0 cannot be modulated"
        );
        assert_eq!(
            (dsp.noise_mask, dsp.echo_mask, dsp.directory_page),
            (0x11, 0x22, 0x33)
        );
    }

    #[test]
    fn the_noise_steps_at_the_rate_flg_names() {
        let mut dsp = SDsp::new();
        dsp.regs[FLG] = 0x1f;
        dsp.latch_keys();
        assert_eq!(dsp.noise_lfsr, 0x2000);
        assert_eq!(next_noise(0b11), 0b1, "equal low bits feed back a 0");
        assert_eq!(next_noise(0b10), 0b100_0000_0000_0001);
        // Rate 0: the generator stands still.
        dsp.regs[FLG] = 0;
        let before = dsp.noise_lfsr;
        dsp.latch_keys();
        assert_eq!(dsp.noise_lfsr, before);
        // Over its full period the register runs through every non-zero
        // 15-bit value once.
        let mut seen = vec![false; 1 << 15];
        let mut lfsr = 0x4000u16;
        for _ in 0..(1 << 15) - 1 {
            assert!(!seen[usize::from(lfsr)]);
            seen[usize::from(lfsr)] = true;
            lfsr = next_noise(lfsr);
        }
        assert_eq!(lfsr, 0x4000);
    }

    #[test]
    fn a_silent_chip_outputs_silence_and_reset_keeps_the_mutes() {
        let mut dsp = SDsp::new();
        let mut aram = vec![0u8; 0x10000];
        for _ in 0..64 {
            assert_eq!(dsp.run_sample(&mut aram), (0, 0));
        }
        dsp.set_voice_mute(3, true);
        dsp.set_addr(0x40);
        dsp.write_data(0x7e);
        dsp.noise_lfsr = 0x1234;
        dsp.reset();
        assert_eq!(dsp.noise_lfsr, 0x4000);
        assert_eq!(dsp.regs()[0x40], 0);
        assert!(dsp.voice_muted(3));
    }

    #[test]
    fn a_savestate_round_trip_is_exact() {
        let mut dsp = SDsp::new();
        let mut aram = vec![0u8; 0x10000];
        // A directory at $0200 pointing at a short looping sample.
        aram[0x0200..0x0204].copy_from_slice(&[0x00, 0x03, 0x00, 0x03]);
        aram[0x0300] = 0xc2; // shift 12, filter 0, loop, not the end
        for b in aram[0x0301..0x0309].iter_mut() {
            *b = 0x79;
        }
        aram[0x0309] = 0xc3; // the last block, looping
        for b in aram[0x030a..0x0312].iter_mut() {
            *b = 0x97;
        }
        for (reg, value) in [
            (VoiceReg::VolL, 0x7f),
            (VoiceReg::VolR, 0x7f),
            (VoiceReg::PitchH, 0x10),
            (VoiceReg::Adsr1, 0x8f),
            (VoiceReg::Adsr2, 0xe0),
        ] {
            dsp.set_vreg(0, reg, value);
        }
        dsp.regs[DIR] = 0x02;
        dsp.regs[registers::MVOLL] = 0x7f;
        dsp.regs[registers::MVOLR] = 0x7f;
        dsp.regs[FLG] = 0;
        dsp.set_addr(KON as u8);
        dsp.write_data(1);
        for _ in 0..40 {
            dsp.run_sample(&mut aram);
        }
        let saved = dsp.to_savestate();
        let mut restored = SDsp::new();
        restored.from_savestate(&saved);
        let mut aram2 = aram.clone();
        for _ in 0..40 {
            assert_eq!(dsp.run_sample(&mut aram), restored.run_sample(&mut aram2));
        }
        assert_eq!(aram, aram2);
        assert_ne!(dsp.voices[0].envelope.level, 0, "the voice is playing");
    }
}
