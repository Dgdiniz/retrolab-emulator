//! Each voice's envelope: a level from 0 to 2047 that the ADSR stages or
//! one of the GAIN modes move once per sample, at a speed set by a rate
//! number from 0 (never) to 31 (every sample).
//!
//! The rates are periods in samples, 2048 for rate 1, halving every
//! three steps along three interleaved series (2048, 1536 and 1280 and
//! their halves), down to 2 and 1 for the last two, and the chip keeps
//! one counter that all of them, and the noise generator, read: a rate's
//! event fires when the counter, shifted by its series' phase, is a
//! multiple of its period. The counter counts down over 30720 samples,
//! the smallest span every period divides.
//!
//! An envelope step decides the level it would move to and the rate it
//! does so at; the level is taken only on a sample where that rate
//! fires. Release ignores the rate and falls by 8 every sample. In ADSR
//! mode the attack rises by 32 (by 1024 at the top rate), decay and
//! sustain fall by one plus a 256th; decay becomes sustain when the top
//! three bits of the level reach the sustain level. In GAIN mode the
//! register either sets the level outright or runs one of four slopes:
//! down by 32, down by one plus a 256th, up by 32, or up by 32 and then
//! by 8 once past 0x600. Whatever the mode, a level that leaves 0..=2047
//! is clamped, and an attack that overflows becomes a decay.
//!
//! Written from fullsnes's ADSR/GAIN and rate tables and from Anomie's
//! notes on the chip's counter; checked against bsnes-plus.

use super::clamp11;

/// A level one step down an exponential decrease: by one, plus a 256th.
#[inline]
fn decayed(level: i32) -> i32 {
    let level = level - 1;
    level - (level >> 8)
}

/// Samples between two events of each rate; rate 0 never fires.
const RATE_PERIOD: [u16; 32] = [
    0, 2048, 1536, 1280, 1024, 768, 640, 512, 384, 320, 256, 192, 160, 128, 96, 80, 64, 48, 40, 32,
    24, 20, 16, 12, 10, 8, 6, 5, 4, 3, 2, 1,
];

/// The three series' phases: where each one's counter stands relative to
/// the shared one. Rates 1, 4, 7, ... are the 2048 series, 2, 5, 8, ... the
/// 1536 series, 3, 6, 9, ... the 1280 series; 30 and 31 ride the first.
const SERIES_PHASE: [u16; 3] = [0, 1040, 536];

/// One full turn of the shared counter.
pub(super) const COUNTER_SPAN: u16 = 30720;

/// The counter after one sample.
#[inline]
pub(super) fn count_down(counter: u16) -> u16 {
    if counter == 0 {
        COUNTER_SPAN - 1
    } else {
        counter - 1
    }
}

/// Whether `rate`'s event falls on this value of the counter.
#[inline]
pub(super) fn rate_fires(counter: u16, rate: u8) -> bool {
    let rate = usize::from(rate & 0x1f);
    if rate == 0 {
        return false;
    }
    let phase = match rate {
        30 | 31 => 0,
        r => SERIES_PHASE[(r - 1) % 3],
    };
    (u32::from(counter) + u32::from(phase)) % u32::from(RATE_PERIOD[rate]) == 0
}

/// Where the envelope is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Stage {
    Release,
    Attack,
    Decay,
    Sustain,
}

impl Stage {
    /// The number the savestate records.
    pub(super) fn code(self) -> u8 {
        match self {
            Stage::Release => 0,
            Stage::Attack => 1,
            Stage::Decay => 2,
            Stage::Sustain => 3,
        }
    }

    /// Back from the savestate; anything unknown is a release, the
    /// silent stage.
    pub(super) fn from_code(code: u8) -> Self {
        match code {
            1 => Stage::Attack,
            2 => Stage::Decay,
            3 => Stage::Sustain,
            _ => Stage::Release,
        }
    }
}

/// One voice's envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Envelope {
    pub stage: Stage,
    /// The current level, 0..=2047.
    pub level: u16,
    /// The level the last step computed before clamping. The bent GAIN
    /// slope reads it to know when to slow down, and it may sit outside
    /// the level's range.
    pub unclamped: i32,
}

impl Envelope {
    pub(super) const SILENT: Envelope = Envelope {
        stage: Stage::Release,
        level: 0,
        unclamped: 0,
    };

    /// The chip's ENVX reading: the level's top seven bits.
    #[inline]
    pub(super) fn envx(&self) -> u8 {
        (self.level >> 4) as u8
    }

    /// Silence at once: back to release with the level at zero.
    pub(super) fn cut(&mut self) {
        self.stage = Stage::Release;
        self.level = 0;
    }

    /// The sample through the envelope: scaled by the level, the result
    /// with bit 0 cleared.
    #[inline]
    pub(super) fn apply(&self, sample: i32) -> i32 {
        ((sample * i32::from(self.level)) >> 11) & !1
    }

    /// One sample of the envelope. `adsr1` is the voice's `$x5` as
    /// latched earlier in the sample (bit 7 selects ADSR; bits 4-6 the
    /// decay rate; bits 0-3 the attack rate), `adsr2` its live `$x6`
    /// (bits 5-7 the sustain level, bits 0-4 the sustain rate), `gain`
    /// its live `$x7`; `counter` is the shared rate counter.
    pub(super) fn step(&mut self, adsr1: u8, adsr2: u8, gain: u8, counter: u16) {
        if self.stage == Stage::Release {
            self.level = self.level.saturating_sub(8);
            return;
        }
        let level = i32::from(self.level);
        let (level, rate, sustain_source) = if adsr1 & 0x80 != 0 {
            self.adsr(level, adsr1, adsr2)
        } else {
            self.gain(level, gain)
        };
        self.settle(level, rate, sustain_source, counter);
    }

    /// The ADSR stages: the level they move to, at which rate, and the
    /// register whose top three bits name the sustain level.
    fn adsr(&self, level: i32, adsr1: u8, adsr2: u8) -> (i32, u8, u8) {
        match self.stage {
            Stage::Attack => {
                let rate = ((adsr1 & 0x0f) << 1) + 1;
                let step = if rate < 31 { 0x20 } else { 0x400 };
                (level + step, rate, adsr2)
            }
            Stage::Decay => (decayed(level), ((adsr1 >> 3) & 0x0e) + 0x10, adsr2),
            _ => (decayed(level), adsr2 & 0x1f, adsr2),
        }
    }

    /// The GAIN register: a level set outright, or one of four slopes.
    /// Here the sustain level comes from GAIN's own top three bits.
    fn gain(&self, level: i32, gain: u8) -> (i32, u8, u8) {
        let level = match gain >> 5 {
            0..=3 => return (i32::from(gain) << 4, 31, gain),
            4 => level - 0x20,
            5 => decayed(level),
            6 => level + 0x20,
            _ => {
                level
                    + if self.unclamped as u32 >= 0x600 {
                        0x8
                    } else {
                        0x20
                    }
            }
        };
        (level, gain & 0x1f, gain)
    }

    /// The common tail: decay turns into sustain at the sustain level, the
    /// unclamped level is kept, a level outside 0..=2047 is clamped and
    /// turns an attack into a decay, and the level is taken only when the
    /// rate fires.
    fn settle(&mut self, mut level: i32, rate: u8, sustain_source: u8, counter: u16) {
        if self.stage == Stage::Decay && (level >> 8) == i32::from(sustain_source >> 5) {
            self.stage = Stage::Sustain;
        }
        self.unclamped = level;
        if level as u32 > 0x7ff {
            level = clamp11(level);
            if self.stage == Stage::Attack {
                self.stage = Stage::Decay;
            }
        }
        if rate_fires(counter, rate) {
            self.level = level as u16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_periods_halve_every_three_rates_along_three_series() {
        for rate in 1..=26usize {
            assert_eq!(
                RATE_PERIOD[rate] / 2,
                RATE_PERIOD[rate + 3],
                "rate {rate} and {}",
                rate + 3
            );
        }
        assert_eq!(&RATE_PERIOD[29..], &[3, 2, 1]);
        assert!(RATE_PERIOD[1..]
            .iter()
            .all(|p| COUNTER_SPAN.is_multiple_of(*p)));
    }

    #[test]
    fn rate_0_never_fires_and_rate_31_always_does() {
        for counter in 0..COUNTER_SPAN {
            assert!(!rate_fires(counter, 0));
            assert!(rate_fires(counter, 31));
        }
    }

    #[test]
    fn a_rate_fires_once_per_period() {
        for rate in 1..=29u8 {
            let hits = (0..COUNTER_SPAN).filter(|&c| rate_fires(c, rate)).count();
            assert_eq!(
                hits,
                usize::from(COUNTER_SPAN / RATE_PERIOD[usize::from(rate)]),
                "rate {rate}"
            );
        }
        // The three series are out of phase with one another.
        assert!(rate_fires(0, 1));
        assert!(!rate_fires(0, 2));
        assert!(rate_fires(496, 2)); // 496 + 1040 = 1536
        assert!(!rate_fires(0, 3));
        assert!(rate_fires(744, 3)); // 744 + 536 = 1280
    }

    #[test]
    fn the_counter_counts_down_and_wraps() {
        assert_eq!(count_down(5), 4);
        assert_eq!(count_down(0), COUNTER_SPAN - 1);
    }

    #[test]
    fn release_falls_by_eight_and_stops_at_zero() {
        let mut env = Envelope {
            stage: Stage::Release,
            level: 100,
            unclamped: 0,
        };
        env.step(0x8f, 0, 0, 0);
        assert_eq!(env.level, 92);
        env.level = 4;
        env.step(0x8f, 0, 0, 0);
        assert_eq!(env.level, 0);
    }

    #[test]
    fn attack_rises_by_1024_at_the_top_rate() {
        let mut env = Envelope {
            stage: Stage::Attack,
            level: 0,
            unclamped: 0,
        };
        // Attack rate 15 -> rate 31: 0x400 per sample, taken every sample.
        env.step(0x8f, 0, 0, 0);
        assert_eq!(env.level, 0x400);
    }

    #[test]
    fn an_attack_past_the_top_clamps_and_becomes_a_decay() {
        let mut env = Envelope {
            stage: Stage::Attack,
            level: 0x600,
            unclamped: 0,
        };
        env.step(0x8f, 0, 0, 0);
        assert_eq!(env.level, 0x7ff);
        assert_eq!(env.stage, Stage::Decay);
        assert_eq!(env.unclamped, 0xa00);
    }

    #[test]
    fn the_stage_moves_even_when_the_rate_does_not_fire() {
        // Attack rate 0 -> rate 1 (period 2048): the level is not taken
        // on a counter the rate misses, but the stage change is.
        let mut env = Envelope {
            stage: Stage::Attack,
            level: 0x7f0,
            unclamped: 0,
        };
        env.step(0x80, 0, 0, 1);
        assert_eq!(env.level, 0x7f0);
        assert_eq!(env.stage, Stage::Decay);
    }

    #[test]
    fn decay_becomes_sustain_at_the_sustain_level() {
        let mut env = Envelope {
            stage: Stage::Decay,
            level: 0x301,
            unclamped: 0,
        };
        // Sustain level 2 (ADSR2 bits 5-7): reached when level >> 8 == 2.
        env.step(0x80, 0x40, 0, 0);
        assert_eq!(env.stage, Stage::Sustain);
    }

    #[test]
    fn gain_direct_sets_the_level_at_once() {
        let mut env = Envelope {
            stage: Stage::Decay,
            level: 0,
            unclamped: 0,
        };
        env.step(0x00, 0, 0x7f, 0);
        assert_eq!(env.level, 0x7f << 4);
    }

    #[test]
    fn the_bent_slope_slows_down_past_0x600() {
        let mut env = Envelope {
            stage: Stage::Attack,
            level: 0x5ff,
            unclamped: 0x5ff,
        };
        // GAIN mode 7 at rate 31: +0x20 below 0x600 ...
        env.step(0x00, 0, 0xff, 0);
        assert_eq!(env.level, 0x61f);
        // ... and +8 once the unclamped level is past it.
        env.step(0x00, 0, 0xff, 0);
        assert_eq!(env.level, 0x627);
    }

    #[test]
    fn a_linear_decrease_below_zero_clamps_to_zero() {
        let mut env = Envelope {
            stage: Stage::Sustain,
            level: 0x10,
            unclamped: 0,
        };
        env.step(0x00, 0, 0x9f, 0); // GAIN mode 4, rate 31
        assert_eq!(env.level, 0);
        assert_eq!(env.unclamped, -0x10);
    }

    #[test]
    fn stage_codes_round_trip() {
        for stage in [Stage::Release, Stage::Attack, Stage::Decay, Stage::Sustain] {
            assert_eq!(Stage::from_code(stage.code()), stage);
        }
        assert_eq!(Stage::from_code(9), Stage::Release);
    }
}
