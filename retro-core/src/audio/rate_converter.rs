//! Integer-ratio sample-rate conversion with zero drift.
//!
//! Converts an absolute CPU/master cycle count into a cumulative
//! audio-sample count using strictly integer arithmetic. Designed for
//! emulator audio paths where any rounding bias eventually shows up
//! as audible drift (samples slowly running ahead of or behind real
//! time).
//!
//! The contract is: starting from a `reset()` anchor, the function
//! `cycles_to_samples(c)` returns exactly the floor of
//! `(output_rate / input_rate) * (c - anchor)` cumulative samples.
//! When the caller periodically calls `advance(c, samples_consumed)`
//! to re-anchor, the converter carries the fractional remainder
//! forward in an internal `remainder` field so successive
//! `cycles_to_samples` calls match what a single big-arithmetic
//! computation would have produced, bit-for-bit, even over hours of
//! emulation.
//!
//! No `f64` is used on the critical path; that is the only way to
//! guarantee deterministic behavior across hosts and platforms.

/// Greatest common divisor (Euclidean algorithm). Used to simplify
/// the (num, den) ratio so multiplications stay small.
const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Tracks the relationship between CPU/master cycles and audio
/// samples without drift.
///
/// `num` is the output sample rate (e.g. `32_000` for the SNES DSP),
/// `den` is the input clock rate (e.g. `21_477_270` for SNES NTSC
/// master clock). Both must be `> 0`. They are simplified by their
/// greatest common divisor at construction time so internal
/// multiplications stay small.
#[derive(Debug, Clone)]
pub struct IntegerRateConverter {
    num: u64,
    den: u64,
    /// Anchor cycle. After `reset(c)` or `advance(c, _)` this equals
    /// `c`; `cycles_to_samples(c2)` works in terms of
    /// `c2 - reference_cycles`.
    reference_cycles: u64,
    /// Total samples consumed up to the anchor.
    samples_at_reference: u64,
    /// Fractional residue from the last division, `0 <= remainder < den`.
    /// Carried forward to make successive `cycles_to_samples` calls
    /// drift-free.
    remainder: u64,
}

impl IntegerRateConverter {
    /// Construct a converter from output sample rate `num` and input
    /// clock rate `den`. Both must be positive.
    ///
    /// The ratio is simplified by `gcd(num, den)` to keep the
    /// per-call multiplication small (e.g. 32_000 / 21_477_270
    /// simplifies to 3200 / 2_147_727).
    pub fn new(num: u64, den: u64) -> Self {
        assert!(num > 0, "output rate must be > 0");
        assert!(den > 0, "input rate must be > 0");
        let g = gcd(num, den);
        Self {
            num: num / g,
            den: den / g,
            reference_cycles: 0,
            samples_at_reference: 0,
            remainder: 0,
        }
    }

    /// Reconstruct a converter from a previously captured state
    /// triple `(reference_cycles, samples_at_reference, remainder)`.
    ///
    /// Used by savestate restore paths so the cumulative sample
    /// counter and fractional remainder survive a save/load cycle
    /// intact, i.e. no audible drift discontinuity at the load
    /// boundary.
    ///
    /// `num` and `den` are re-passed (rather than persisted) so the
    /// ratio is always re-derived from the current console region;
    /// `remainder` is **clamped** with a debug-assert to stay in the
    /// invariant `0 <= remainder < den_simplified`, defending against
    /// savestates produced with a different denominator (e.g. region
    /// changed between save and load).
    pub fn with_state(
        num: u64,
        den: u64,
        reference_cycles: u64,
        samples_at_reference: u64,
        remainder: u64,
    ) -> Self {
        let mut conv = Self::new(num, den);
        debug_assert!(
            remainder < conv.den,
            "remainder ({}) must be < simplified denominator ({}); savestate may have been produced with a different region",
            remainder,
            conv.den,
        );
        conv.reference_cycles = reference_cycles;
        conv.samples_at_reference = samples_at_reference;
        // Be lenient at release time: clamp out-of-range remainder so
        // a bad savestate degrades to "one missing fractional sample"
        // instead of multiplication overflow.
        conv.remainder = remainder.min(conv.den.saturating_sub(1));
        conv
    }

    /// The simplified numerator (output rate / gcd).
    pub fn num(&self) -> u64 {
        self.num
    }

    /// The simplified denominator (input rate / gcd).
    pub fn den(&self) -> u64 {
        self.den
    }

    /// Cumulative sample count that should have been emitted by
    /// absolute cycle `cycles`, measured from the last `reset` /
    /// `advance` anchor.
    ///
    /// Pure function, does not mutate. `cycles` must be `>=` the
    /// last anchor cycle; the assert catches caller bugs in debug
    /// builds.
    ///
    /// # Example
    ///
    /// ```
    /// use retro_core::audio::IntegerRateConverter;
    ///
    /// // SNES master clock (NTSC, 21.47727 MHz) -> DSP output at 32 kHz.
    /// // After 30 seconds the DSP must have emitted exactly
    /// // 32_000 * 30 = 960_000 samples, no drift, no rounding bias.
    /// let mut conv = IntegerRateConverter::new(32_000, 21_477_270);
    /// for sec in 1u64..=30 {
    ///     let cycles = sec * 21_477_270;
    ///     let cumulative = conv.cycles_to_samples(cycles);
    ///     conv.advance(cycles, cumulative);
    /// }
    /// assert_eq!(
    ///     conv.cycles_to_samples(30 * 21_477_270),
    ///     960_000,
    ///     "30 s of 32 kHz audio must be exactly 960_000 samples"
    /// );
    /// ```
    pub fn cycles_to_samples(&self, cycles: u64) -> u64 {
        debug_assert!(
            cycles >= self.reference_cycles,
            "cycles ({}) must not go backward below the reference ({})",
            cycles,
            self.reference_cycles,
        );
        let delta = cycles - self.reference_cycles;
        let weighted = self.num * delta + self.remainder;
        self.samples_at_reference + weighted / self.den
    }

    /// Re-anchor the converter at `cycles`, declaring that
    /// `samples_consumed` cumulative samples have been consumed by
    /// this point.
    ///
    /// The fractional remainder is carried forward in `remainder` so
    /// future `cycles_to_samples` calls remain drift-free. If the
    /// caller passes a `samples_consumed` value that matches what
    /// `cycles_to_samples(cycles)` would have returned (the common
    /// case), no information is lost.
    pub fn advance(&mut self, cycles: u64, samples_consumed: u64) {
        debug_assert!(
            cycles >= self.reference_cycles,
            "cycles ({}) must not go backward below the reference ({})",
            cycles,
            self.reference_cycles,
        );
        let delta = cycles - self.reference_cycles;
        let weighted = self.num * delta + self.remainder;
        let leftover = weighted % self.den;
        self.reference_cycles = cycles;
        self.samples_at_reference = samples_consumed;
        self.remainder = leftover;
    }

    /// Hard reset to a new anchor cycle, clearing the cumulative
    /// sample counter and the remainder. Typically called on console
    /// reset (with `cycles = 0`) or when restoring a savestate.
    pub fn reset(&mut self, cycles: u64) {
        self.reference_cycles = cycles;
        self.samples_at_reference = 0;
        self.remainder = 0;
    }

    /// Current anchor cycle (for diagnostics / tests).
    pub fn reference_cycles(&self) -> u64 {
        self.reference_cycles
    }

    /// Current cumulative sample count at the anchor.
    pub fn samples_at_reference(&self) -> u64 {
        self.samples_at_reference
    }

    /// Current fractional remainder (`0 <= remainder < den`).
    pub fn remainder(&self) -> u64 {
        self.remainder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcd_basic() {
        assert_eq!(gcd(12, 18), 6);
        assert_eq!(gcd(17, 5), 1);
        assert_eq!(gcd(100, 0), 100);
        assert_eq!(gcd(0, 100), 100);
    }

    #[test]
    fn new_simplifies_ratio_by_gcd() {
        let conv = IntegerRateConverter::new(32_000, 21_477_270);
        // gcd(32000, 21477270) = 10
        assert_eq!(conv.num(), 3_200);
        assert_eq!(conv.den(), 2_147_727);
    }

    #[test]
    #[should_panic(expected = "output rate must be > 0")]
    fn new_rejects_zero_num() {
        IntegerRateConverter::new(0, 100);
    }

    #[test]
    #[should_panic(expected = "input rate must be > 0")]
    fn new_rejects_zero_den() {
        IntegerRateConverter::new(100, 0);
    }

    #[test]
    fn cycles_to_samples_at_anchor_is_zero() {
        let conv = IntegerRateConverter::new(32_000, 21_477_270);
        assert_eq!(conv.cycles_to_samples(0), 0);
    }

    #[test]
    fn cycles_to_samples_exact_snes_ntsc_30s() {
        let conv = IntegerRateConverter::new(32_000, 21_477_270);
        // Exactly 30 seconds of master cycles
        let cycles = 30u64 * 21_477_270;
        assert_eq!(conv.cycles_to_samples(cycles), 960_000);
    }

    #[test]
    fn chunked_advance_matches_oneshot_compute() {
        // Pick an irregular set of chunk sizes that exercise the
        // remainder accumulator: their sum is not aligned to the
        // sample period, so any drift bug would show up.
        let chunk_sizes: [u64; 7] = [123_456, 789, 1_000_000, 17, 9_999_999, 555_555, 42_000_001];
        let total_cycles: u64 = chunk_sizes.iter().sum();

        let mut conv = IntegerRateConverter::new(48_000, 21_477_270);
        let mut acc = 0u64;
        for &chunk in &chunk_sizes {
            acc += chunk;
            let cumulative = conv.cycles_to_samples(acc);
            conv.advance(acc, cumulative);
        }

        let oneshot = IntegerRateConverter::new(48_000, 21_477_270);
        assert_eq!(
            conv.samples_at_reference(),
            oneshot.cycles_to_samples(total_cycles),
            "chunked advance must produce the same cumulative sample count as a single big compute"
        );
    }

    #[test]
    fn no_drift_after_one_hour_of_snes_audio() {
        // 1 hour of NTSC SNES master clock, advanced 1 ms at a time
        // (typical: advance happens at port-write events; 1 ms is a
        // realistic upper bound on how long between catch-ups).
        let mut conv = IntegerRateConverter::new(32_000, 21_477_270);
        let cycles_per_ms = 21_477_270u64 / 1000; // = 21477 (not exact)
        let mut acc = 0u64;
        for _ in 0..(60 * 60 * 1000) {
            acc += cycles_per_ms;
            let cumulative = conv.cycles_to_samples(acc);
            conv.advance(acc, cumulative);
        }
        // Compare with one-shot from a fresh converter
        let fresh = IntegerRateConverter::new(32_000, 21_477_270);
        assert_eq!(
            conv.samples_at_reference(),
            fresh.cycles_to_samples(acc),
            "no drift after 1 hour, even when chunking by inexact cycles_per_ms"
        );
    }

    #[test]
    fn reset_clears_anchor_and_counters() {
        let mut conv = IntegerRateConverter::new(32_000, 21_477_270);
        conv.advance(1_000_000, 1490);
        conv.reset(0);
        assert_eq!(conv.reference_cycles(), 0);
        assert_eq!(conv.samples_at_reference(), 0);
        assert_eq!(conv.remainder(), 0);
        assert_eq!(conv.cycles_to_samples(0), 0);
    }

    #[test]
    fn reset_to_nonzero_anchor() {
        let mut conv = IntegerRateConverter::new(32_000, 21_477_270);
        conv.reset(100);
        // Querying at the anchor returns 0; querying past returns the
        // accumulated samples from delta only.
        assert_eq!(conv.cycles_to_samples(100), 0);
        assert_eq!(
            conv.cycles_to_samples(100 + 21_477_270),
            32_000,
            "after reset, 1 second of cycles emits 32_000 samples"
        );
    }

    #[test]
    fn advance_does_not_lose_samples_across_calls() {
        // Tight loop with tiny advances, common pattern when
        // catching up at every port write.
        let mut conv = IntegerRateConverter::new(32_040, 21_477_273);
        let mut acc = 0u64;
        for _ in 0..100_000 {
            acc += 1; // 1-cycle step
            let cumulative = conv.cycles_to_samples(acc);
            conv.advance(acc, cumulative);
        }
        let fresh = IntegerRateConverter::new(32_040, 21_477_273);
        assert_eq!(
            conv.samples_at_reference(),
            fresh.cycles_to_samples(acc),
            "1-cycle advances must accumulate to the same sample count as a single compute"
        );
    }

    #[test]
    fn nondivisible_ratio_keeps_remainder_in_range() {
        let mut conv = IntegerRateConverter::new(7, 11);
        conv.advance(100, conv.cycles_to_samples(100));
        assert!(
            conv.remainder() < conv.den(),
            "remainder ({}) must stay < den ({})",
            conv.remainder(),
            conv.den()
        );
    }

    #[test]
    fn with_state_round_trips_through_getters() {
        // Drive a converter to a non-trivial, irregular state, snapshot
        // (ref_cycles, samples, remainder), reconstruct via with_state,
        // and confirm future computations match the original bit-for-bit.
        let mut original = IntegerRateConverter::new(44_100, 53_693_175);
        let chunks: [u64; 5] = [123_456, 789, 1_000_000, 17, 9_999_999];
        let mut acc = 0u64;
        for &c in &chunks {
            acc += c;
            let cumulative = original.cycles_to_samples(acc);
            original.advance(acc, cumulative);
        }

        let restored = IntegerRateConverter::with_state(
            44_100,
            53_693_175,
            original.reference_cycles(),
            original.samples_at_reference(),
            original.remainder(),
        );

        assert_eq!(restored.reference_cycles(), original.reference_cycles());
        assert_eq!(
            restored.samples_at_reference(),
            original.samples_at_reference()
        );
        assert_eq!(restored.remainder(), original.remainder());
        assert_eq!(restored.num(), original.num());
        assert_eq!(restored.den(), original.den());

        // Future queries from a probe cycle must agree.
        let probe = acc + 7_654_321;
        assert_eq!(
            restored.cycles_to_samples(probe),
            original.cycles_to_samples(probe),
            "with_state must reconstruct a converter behaviorally identical to the original"
        );
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn with_state_clamps_out_of_range_remainder_in_release() {
        // A savestate produced with one denominator then loaded with
        // a smaller denominator (e.g. region change between save and
        // load) would have remainder >= den. In release, with_state
        // clamps silently to den-1; in debug, the debug_assert fires.
        let conv = IntegerRateConverter::with_state(7, 11, 100, 50, 999_999);
        assert!(conv.remainder() < conv.den());
    }

    #[test]
    fn samples_grow_monotonically() {
        let conv = IntegerRateConverter::new(32_000, 21_477_270);
        let mut last = 0;
        for c in (0..=10_000_000).step_by(12345) {
            let s = conv.cycles_to_samples(c);
            assert!(
                s >= last,
                "samples must not decrease: {s} < {last} at c={c}"
            );
            last = s;
        }
    }
}
