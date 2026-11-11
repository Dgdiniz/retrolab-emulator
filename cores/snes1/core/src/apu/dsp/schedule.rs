//! The chip's timetable: one sample takes 32 clocks, and the one datapath
//! serves the eight voices in turn, three clocks apart, each voice's pass
//! spread over twelve clocks so that three voices are in flight at any
//! clock. The echo unit, the register latches and the output take the
//! last ten clocks. The order within a clock is the order the entries
//! are listed in.
//!
//! Voice 0's pass straddles the sample: it reads its directory and
//! sample bytes at the end of one sample, and mixes at the start of the
//! next. That is also why its third stage is split in three (clocks 22,
//! 25 and 30) where voices 1-7 run the three parts in one clock.
//!
//! The timetable is the hardware's, as measured in Anomie's notes; this
//! table is how it is written down here.

/// One step of a voice's pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VoiceStage {
    /// Latch the voice's source number; form the directory address of
    /// the voice before it.
    LatchSource,
    /// Read the start or loop address from the directory, latch the
    /// envelope control byte and the low pitch byte.
    ReadDirectory,
    /// Add the high pitch byte.
    PitchHigh,
    /// Read the current block's header and the next data byte.
    FetchBlock,
    /// Pitch modulation, key-on set-up, interpolation or noise, the
    /// envelope, the key-off / key-on latches.
    Synthesize,
    /// All three of the above in one clock (voices 1-7).
    Stage3,
    /// Decode four more samples when they are due, advance the position,
    /// mix the left channel.
    DecodeMixLeft,
    /// Mix the right channel; shadow ENDX.
    MixRight,
    /// Shadow OUTX.
    ShadowOutx,
    /// Write ENDX; shadow ENVX.
    WriteEndx,
    /// Write OUTX.
    WriteOutx,
    /// Write ENVX.
    WriteEnvx,
}

/// One entry of the timetable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Voice(u8, VoiceStage),
    /// Move the echo history on, point at this sample's slot in the ARAM
    /// ring, read the left sample into the history.
    EchoLeftIn,
    /// Read the right sample into the history.
    EchoRightIn,
    /// The eight-tap filter over the history.
    EchoFilter,
    /// Keep the left output; feed the filtered echo back into the sums
    /// to be written.
    EchoFeedback,
    /// Latch the pitch-modulation enables.
    LatchPitchMod,
    /// The sample leaves the chip.
    Output,
    /// Latch the noise and echo enables and the directory page.
    LatchRouting,
    /// Latch the flags that gate the echo write.
    LatchEchoFlags,
    /// Alternate the samples that honour KON and KOFF.
    ToggleKeySample,
    /// Latch the echo page, step the ring, write the left sample back.
    EchoAdvance,
    /// Latch KON and KOFF on a key sample; count the rate counter down;
    /// clock the noise.
    LatchKeys,
    /// Write the right sample back.
    EchoWriteRight,
}

const fn v(voice: u8, stage: VoiceStage) -> Step {
    Step::Voice(voice, stage)
}

use Step::*;
use VoiceStage::*;

/// One list, two readings: the `SCHEDULE` table the tests check, and
/// `SDsp::run_sample`, the same steps as straight-line code, because a
/// loop over the table would dispatch every step at run time and the
/// chip runs this 32 000 times a second.
macro_rules! timetable {
    ($( $clock:literal : [ $( $step:expr ),* ] ),* $(,)?) => {
        /// The 32 clocks of a sample, in order, and the steps of each,
        /// what the tests check; the sample below is the same list.
        #[allow(dead_code)]
        pub(super) const SCHEDULE: [&[Step]; 32] = [ $( &[ $( $step ),* ] ),* ];

        impl super::SDsp {
            /// One sample: the timetable, clock by clock, step by step.
            pub(in crate::apu) fn run_sample(&mut self, aram: &mut [u8]) -> (i16, i16) {
                let mut out = (0, 0);
                $( $( self.step($step, aram, &mut out); )* )*
                out
            }
        }
    };
}

timetable! {
    0: [v(0, MixRight), v(1, ReadDirectory)],
    1: [v(0, ShadowOutx), v(1, Stage3)],
    2: [v(0, WriteEndx), v(1, DecodeMixLeft), v(3, LatchSource)],
    3: [v(0, WriteOutx), v(1, MixRight), v(2, ReadDirectory)],
    4: [v(0, WriteEnvx), v(1, ShadowOutx), v(2, Stage3)],
    5: [v(1, WriteEndx), v(2, DecodeMixLeft), v(4, LatchSource)],
    6: [v(1, WriteOutx), v(2, MixRight), v(3, ReadDirectory)],
    7: [v(1, WriteEnvx), v(2, ShadowOutx), v(3, Stage3)],
    8: [v(2, WriteEndx), v(3, DecodeMixLeft), v(5, LatchSource)],
    9: [v(2, WriteOutx), v(3, MixRight), v(4, ReadDirectory)],
    10: [v(2, WriteEnvx), v(3, ShadowOutx), v(4, Stage3)],
    11: [v(3, WriteEndx), v(4, DecodeMixLeft), v(6, LatchSource)],
    12: [v(3, WriteOutx), v(4, MixRight), v(5, ReadDirectory)],
    13: [v(3, WriteEnvx), v(4, ShadowOutx), v(5, Stage3)],
    14: [v(4, WriteEndx), v(5, DecodeMixLeft), v(7, LatchSource)],
    15: [v(4, WriteOutx), v(5, MixRight), v(6, ReadDirectory)],
    16: [v(4, WriteEnvx), v(5, ShadowOutx), v(6, Stage3)],
    17: [v(0, LatchSource), v(5, WriteEndx), v(6, DecodeMixLeft)],
    18: [v(5, WriteOutx), v(6, MixRight), v(7, ReadDirectory)],
    19: [v(5, WriteEnvx), v(6, ShadowOutx), v(7, Stage3)],
    20: [v(1, LatchSource), v(6, WriteEndx), v(7, DecodeMixLeft)],
    21: [v(6, WriteOutx), v(7, MixRight), v(0, ReadDirectory)],
    22: [v(0, PitchHigh), v(6, WriteEnvx), v(7, ShadowOutx), EchoLeftIn],
    23: [v(7, WriteEndx), EchoRightIn],
    24: [v(7, WriteOutx)],
    25: [v(0, FetchBlock), v(7, WriteEnvx), EchoFilter],
    26: [EchoFeedback],
    27: [LatchPitchMod, Output],
    28: [LatchRouting, LatchEchoFlags],
    29: [ToggleKeySample, EchoAdvance],
    30: [LatchKeys, v(0, Synthesize), EchoWriteRight],
    31: [v(0, DecodeMixLeft), v(2, LatchSource)],
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every (clock, step) of the table, voice stages with stage 3
    /// expanded into its three parts.
    fn steps() -> Vec<(usize, Step)> {
        let mut out = Vec::new();
        for (clock, entries) in SCHEDULE.iter().enumerate() {
            for &step in entries.iter() {
                match step {
                    Voice(n, Stage3) => {
                        out.push((clock, v(n, PitchHigh)));
                        out.push((clock, v(n, FetchBlock)));
                        out.push((clock, v(n, Synthesize)));
                    }
                    s => out.push((clock, s)),
                }
            }
        }
        out
    }

    const PASS: [VoiceStage; 11] = [
        LatchSource,
        ReadDirectory,
        PitchHigh,
        FetchBlock,
        Synthesize,
        DecodeMixLeft,
        MixRight,
        ShadowOutx,
        WriteEndx,
        WriteOutx,
        WriteEnvx,
    ];

    #[test]
    fn every_voice_runs_each_stage_exactly_once_per_sample() {
        let steps = steps();
        for voice in 0..8u8 {
            for stage in PASS {
                let n = steps.iter().filter(|(_, s)| *s == v(voice, stage)).count();
                assert_eq!(n, 1, "voice {voice} {stage:?}");
            }
        }
    }

    #[test]
    fn the_voices_are_three_clocks_apart_and_a_pass_is_in_order() {
        let steps = steps();
        let clock_of = |voice: u8, stage: VoiceStage| -> usize {
            steps.iter().find(|(_, s)| *s == v(voice, stage)).unwrap().0
        };
        // From the directory read on, every voice's pass is in stage
        // order, one clock per stage (stage 3's parts share a clock for
        // voices 1-7), and voice n runs three clocks after voice n-1.
        for voice in 1..8u8 {
            let start = clock_of(voice, ReadDirectory);
            assert_eq!(start, (3 * usize::from(voice) + 29) % 32, "voice {voice}");
            let mut expect = start;
            for stage in &PASS[1..] {
                let clock = clock_of(voice, *stage);
                assert_eq!(clock, expect % 32, "voice {voice} {stage:?}");
                if !matches!(stage, PitchHigh | FetchBlock) {
                    expect += 1;
                }
            }
        }
        // Voice 0 reads its directory at clock 21 and mixes at 31 and 0:
        // its pass crosses the sample boundary.
        assert_eq!(clock_of(0, ReadDirectory), 21);
        assert_eq!(clock_of(0, PitchHigh), 22);
        assert_eq!(clock_of(0, FetchBlock), 25);
        assert_eq!(clock_of(0, Synthesize), 30);
        assert_eq!(clock_of(0, DecodeMixLeft), 31);
        assert_eq!(clock_of(0, MixRight), 0);
        assert_eq!(clock_of(0, WriteEnvx), 4);
    }

    #[test]
    fn the_global_steps_each_run_once_in_the_last_ten_clocks() {
        let steps = steps();
        for (step, clock) in [
            (EchoLeftIn, 22),
            (EchoRightIn, 23),
            (EchoFilter, 25),
            (EchoFeedback, 26),
            (LatchPitchMod, 27),
            (Output, 27),
            (LatchRouting, 28),
            (LatchEchoFlags, 28),
            (ToggleKeySample, 29),
            (EchoAdvance, 29),
            (LatchKeys, 30),
            (EchoWriteRight, 30),
        ] {
            let at: Vec<usize> = steps
                .iter()
                .filter(|(_, s)| *s == step)
                .map(|(c, _)| *c)
                .collect();
            assert_eq!(at, [clock], "{step:?}");
        }
    }

    #[test]
    fn the_key_latches_come_before_voice_0_synthesizes() {
        assert_eq!(SCHEDULE[30], &[LatchKeys, v(0, Synthesize), EchoWriteRight]);
    }
}
