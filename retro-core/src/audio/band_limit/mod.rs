//! A sound chip's output as the host's samples, band-limited: nothing of
//! what lies above half the host's rate folds back into what it hears.
//!
//! A chip's output reaches the host in one of two forms:
//!
//! - a level that changes at exact times (a square-wave generator's
//!   output, a value a DAC holds until the next one): [`BandLimitedSteps`],
//!   each change a step whose derivative is a band-limited impulse;
//! - samples made at the chip's own rate, below the host's, taken as the
//!   samples of a band-limited signal and read at the host's sample times:
//!   [`SincResampler`].
//!
//! Both use one kernel, [`SincKernel`]: a Kaiser-windowed sinc low-pass,
//! tabulated at fractions of a sample and interpolated linearly between
//! them, each fraction's taps normalised so a constant passes exactly.
//! Every coefficient is worked out with addition, subtraction,
//! multiplication, division and square roots alone, so a native build and
//! a WebAssembly build make the same samples. [`OutputFilter`] is a
//! console's analog stage after them, a one-pole low-pass and a one-pole
//! DC-blocking high-pass. Where a chip's time lands on the host's samples
//! is the converter's: [`crate::audio::IntegerRateConverter::position`].
//!
//! The methods come from the literature, not from another emulator:
//! J. O. Smith and P. Gossett, "A flexible sampling-rate conversion
//! method" (ICASSP 1984), and J. O. Smith's pages on digital audio
//! resampling (the tabulated, interpolated sinc); T. Stilson and J. O.
//! Smith, "Alias-free digital synthesis of classic analog waveforms" (ICMC
//! 1996: the band-limited impulse train, integrated to band-limited
//! steps); E. Brandt, "Hard sync without aliasing" (ICMC 2001: a
//! band-limited step as the ideal one plus its short difference from it);
//! J. F. Kaiser, "Nonrecursive digital filter design using the I0-sinh
//! window function" (ISCAS 1974), with A. V. Oppenheim and R. W. Schafer's
//! Discrete-Time Signal Processing (the window's beta from the stopband,
//! the one-pole filters).
//!
//! None of it is machine state: the stage is the host's reading of the
//! chips' output, which no game reads back, so it is in no savestate and no
//! state hash.

mod filter;
mod kernel;
mod math;
mod resampler;
#[cfg(test)]
mod spectrum;
mod steps;

pub use filter::OutputFilter;
pub use kernel::SincKernel;
pub use resampler::SincResampler;
pub use steps::BandLimitedSteps;
