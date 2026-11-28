//! The chip's DAC: what each channel reaches each side of the output as.
//!
//! Both chips hand each channel to the DAC as a 9-bit number, the
//! operator's 14-bit result with its 5 low bits cleared, so a channel
//! faded below one step keeps toggling about it instead of fading out.
//! The discrete YM2612 of the first consoles misses its zero: its DAC's
//! levels either side of zero lie further apart than the others, the
//! negative ones 3 of its steps lower and the others 4 higher (the
//! levels of a channel output one chip cycle in four, the rest of its
//! slot held at the level of its sign, averaged over the slot). A quiet
//! channel keeps a floor it toggles about.
//!
//! The pan only says which sides take the channel's own cycle. A side
//! the channel is not panned to takes the level of its sign for the
//! whole slot on the discrete chip, 4 steps down below zero and 4 up
//! from zero on: a channel panned to one side leaves a faint square of
//! its sign on the other, and a silent one sits 4 steps up on every
//! side. The YM3438 the later consoles integrate takes the 9-bit steps
//! alone, and puts nothing on a side a channel is not panned to.
//!
//! The form, the 9-bit mask and the levels either side of zero on both
//! sides, follows ares (`component/audio/ym2612/ym2612.cpp`, its
//! `outMask` and the "crossover distortion" offset), ISC, the notice is
//! at the end of `LICENSE`.

/// The DAC a channel's output goes through: which chip the console has.
/// A host setting: it changes no state the machine holds, and the
/// savestate and the state hash read nothing of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FmDacModel {
    /// The discrete YM2612 (the first consoles): the 9-bit steps, the gap
    /// at zero, and the level of a channel's sign on a side it is not
    /// panned to.
    #[default]
    Discrete,
    /// The YM3438 the later consoles integrate: the 9-bit steps alone, and
    /// nothing on a side a channel is not panned to.
    Integrated,
}

/// One step of the DAC's 9 bits, in the 14 bits of an operator's result.
const STEP: i32 = 1 << 5;

impl FmDacModel {
    /// The model's name: `discrete` or `integrated`.
    pub fn name(self) -> &'static str {
        match self {
            FmDacModel::Discrete => "discrete",
            FmDacModel::Integrated => "integrated",
        }
    }

    /// The model named `name` (`discrete` or `integrated`), or `None`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "discrete" => Some(FmDacModel::Discrete),
            "integrated" => Some(FmDacModel::Integrated),
            _ => None,
        }
    }

    /// A channel's 14-bit output as the DAC gives it to one side, in the
    /// same units; `panned`: whether the channel is panned to that side.
    #[inline]
    pub fn output(self, value: i32, panned: bool) -> i32 {
        let nine_bits = value & -STEP;
        match self {
            FmDacModel::Integrated if panned => nine_bits,
            FmDacModel::Integrated => 0,
            FmDacModel::Discrete => {
                // Every side takes the level of the channel's sign, a side
                // it is panned to its 9 bits too, one step up below zero.
                let (value_level, sign_level) = if nine_bits < 0 {
                    (nine_bits + STEP, -4 * STEP)
                } else {
                    (nine_bits, 4 * STEP)
                };
                if panned {
                    value_level + sign_level
                } else {
                    sign_level
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 9 bits on both chips: the 5 low bits of the 14 cleared, toward
    /// minus infinity, as two's complement holds them.
    #[test]
    fn both_chips_take_9_bits() {
        let integrated = FmDacModel::Integrated;
        for (value, nine_bits) in [
            (0, 0),
            (31, 0),
            (32, 32),
            (100, 96),
            (8191, 8160),
            (-1, -32),
            (-32, -32),
            (-33, -64),
            (-8192, -8192),
        ] {
            assert_eq!(integrated.output(value, true), nine_bits, "{value}");
        }
    }

    /// The discrete chip's levels on a side the channel is panned to: 4
    /// steps up from zero on, 3 steps down below it, so the levels either
    /// side of zero, -4 and +4 steps, lie 8 steps apart, where every other
    /// two neighbours lie one apart.
    #[test]
    fn the_discrete_chip_misses_its_zero() {
        let discrete = FmDacModel::Discrete;
        for (value, level) in [
            (0, 4 * STEP),
            (31, 4 * STEP),
            (32, 5 * STEP),
            (-1, -4 * STEP),
            (-32, -4 * STEP),
            (-33, -5 * STEP),
            (8191, 8160 + 4 * STEP),
            (-8192, -8192 - 3 * STEP),
        ] {
            assert_eq!(discrete.output(value, true), level, "{value}");
        }
        let steps: Vec<i32> = (-3..=3)
            .map(|step| discrete.output(step * STEP, true) / STEP)
            .collect();
        assert_eq!(steps, [-6, -5, -4, 4, 5, 6, 7]);
    }

    /// The DAC register's $7F and $80 (channel 6 playing samples, each a
    /// value less 128, raised by 6 bits): one DAC step apart on the
    /// integrated chip, four and a half on the discrete.
    #[test]
    fn the_dac_register_s_middle_on_each_chip() {
        let at = |model: FmDacModel, register: i32| model.output((register - 0x80) << 6, true);
        let dac_step = 2 * STEP;
        let jump = |model| at(model, 0x80) - at(model, 0x7F);
        assert_eq!(jump(FmDacModel::Integrated), dac_step);
        assert_eq!(jump(FmDacModel::Discrete), 9 * dac_step / 2);
        assert_eq!(
            at(FmDacModel::Discrete, 0x81) - at(FmDacModel::Discrete, 0x80),
            dac_step
        );
    }

    /// A side the channel is not panned to: nothing on the integrated chip;
    /// on the discrete one the level of the channel's sign whatever its
    /// size, 4 steps down below zero and 4 up from zero on, which a side
    /// it is panned to carries too, beneath its 9 bits.
    #[test]
    fn the_side_a_channel_is_not_panned_to() {
        let (discrete, integrated) = (FmDacModel::Discrete, FmDacModel::Integrated);
        for value in [0, 31, 32, 8191, -1, -32, -33, -8192] {
            let sign_level = if value < 0 { -4 * STEP } else { 4 * STEP };
            assert_eq!(discrete.output(value, false), sign_level, "{value}");
            assert_eq!(integrated.output(value, false), 0, "{value}");
            let value_level = integrated.output(value, true) + if value < 0 { STEP } else { 0 };
            assert_eq!(
                discrete.output(value, true),
                value_level + sign_level,
                "{value}"
            );
        }
    }

    #[test]
    fn the_names() {
        for model in [FmDacModel::Discrete, FmDacModel::Integrated] {
            assert_eq!(FmDacModel::from_name(model.name()), Some(model));
        }
        assert_eq!(FmDacModel::from_name("YM2612"), None);
        assert_eq!(FmDacModel::default(), FmDacModel::Discrete);
    }
}
