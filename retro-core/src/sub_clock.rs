//! A sub-processor's clock: a processor run at a fixed ratio of the master
//! clock, caught up to it in bursts, its overshoot kept.
//!
//! A console's sub-processors (a sound CPU, a cartridge's coprocessor) run
//! on clocks of their own, each a fixed ratio of the master clock. The
//! machine runs one in bursts: master time passes, and the sub-processor
//! runs the cycles it owes, its last instruction ending past the present.
//! [`SubClock`] keeps that account without drift: the ratio's remainder,
//! the credit, carried from one pass to the next, and the lead, the cycles
//! the sub-processor stands past the present (below 0, what it owes), which
//! the time after pays back. A held sub-processor (in reset, or off its
//! bus) lets its time pass and owes none of it.

use crate::audio::rate_converter::gcd;

/// How a sub-processor's clock divides the master clock's: `num` of its
/// cycles in each `den` cycles of the master clock.
pub trait ClockRatio: Copy {
    /// The sub-processor's cycles in `num`.
    fn num(self) -> u32;
    /// The master clock's cycles in `den`.
    fn den(self) -> u32;
    /// `span` cycles of the master clock on top of `credit` (below the
    /// denominator): the sub-processor's whole cycles in them, and the
    /// credit left, below the denominator.
    fn split(self, span: u64, credit: u32) -> (u64, u32);
    /// The credit of a clock passed from master time 0 to `master`:
    /// `master * num` modulo `den`.
    fn credit_at(self, master: u64) -> u32 {
        let den = u64::from(self.den());
        ((master % den) * u64::from(self.num()) % den) as u32
    }
    /// Where a sub-processor `lead` cycles past the present stands on the
    /// master clock, with `credit` of the present's next cycle: in master
    /// cycles past the present, rounded down, `(lead * den - credit) / num`
    /// (below 0, behind it).
    fn master_lead(self, lead: i64, credit: u32) -> i64;
}

/// A ratio the master clock's own divisors fix, the same in every region:
/// the Mega Drive's Z80 runs at a fifteenth of the master clock and its
/// 68000 at a seventh, so the Z80 runs 7 cycles in 15 of the 68000's. Its
/// arithmetic is 32-bit, for a clock passed a step at a time: a span of at
/// most `(u32::MAX - DEN) / NUM` master cycles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixedRatio<const NUM: u32, const DEN: u32>;

impl<const NUM: u32, const DEN: u32> ClockRatio for FixedRatio<NUM, DEN> {
    #[inline(always)]
    fn num(self) -> u32 {
        NUM
    }

    #[inline(always)]
    fn den(self) -> u32 {
        DEN
    }

    #[inline(always)]
    fn split(self, span: u64, credit: u32) -> (u64, u32) {
        debug_assert!(
            span <= u64::from((u32::MAX - DEN) / NUM),
            "a span of {span} master cycles is past what a fixed ratio's 32-bit arithmetic holds"
        );
        let weighted = span as u32 * NUM + credit;
        (u64::from(weighted / DEN), weighted % DEN)
    }

    #[inline(always)]
    fn master_lead(self, lead: i64, credit: u32) -> i64 {
        debug_assert!(
            lead.unsigned_abs() <= u64::from(i32::MAX as u32 / DEN),
            "a lead of {lead} cycles is past what a fixed ratio's 32-bit arithmetic holds"
        );
        i64::from((lead as i32 * DEN as i32 - credit as i32).div_euclid(NUM as i32))
    }
}

/// A ratio two crystals give, which can differ by region (the SNES's SPC700
/// runs on a crystal of its own, at 1.024 MHz against a master clock of
/// 21.477 MHz or 21.281 MHz): in lowest terms. Its arithmetic is 64-bit,
/// for a clock passed a long span at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio {
    num: u32,
    den: u32,
}

impl Ratio {
    /// `num` sub-processor cycles in each `den` master cycles, in lowest
    /// terms.
    ///
    /// # Panics
    ///
    /// When a term is 0, or past `u32::MAX` in lowest terms.
    pub fn new(num: u64, den: u64) -> Self {
        assert!(num > 0 && den > 0, "a clock ratio's terms must be above 0");
        let g = gcd(num, den);
        let (num, den) = (num / g, den / g);
        Self {
            num: u32::try_from(num).expect("a clock ratio's terms must fit 32 bits"),
            den: u32::try_from(den).expect("a clock ratio's terms must fit 32 bits"),
        }
    }
}

impl ClockRatio for Ratio {
    #[inline]
    fn num(self) -> u32 {
        self.num
    }

    #[inline]
    fn den(self) -> u32 {
        self.den
    }

    #[inline]
    fn split(self, span: u64, credit: u32) -> (u64, u32) {
        let weighted = span * u64::from(self.num) + u64::from(credit);
        let den = u64::from(self.den);
        (weighted / den, (weighted % den) as u32)
    }

    #[inline]
    fn master_lead(self, lead: i64, credit: u32) -> i64 {
        (lead * i64::from(self.den) - i64::from(credit)).div_euclid(i64::from(self.num))
    }
}

/// A sub-processor's clock against the master clock: where master time
/// stands for it, the ratio's credit, and the sub-processor's lead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubClock<R> {
    ratio: R,
    /// The master time the clock was passed to.
    passed_to: u64,
    /// The ratio's remainder, below its denominator: the part of a
    /// sub-processor cycle that the master time passed holds.
    credit: u32,
    /// The sub-processor's cycles past the present, what its last
    /// instruction ran beyond it; below 0, the cycles it owes.
    lead: i64,
}

impl<R: ClockRatio> SubClock<R> {
    /// The clock at master time 0, with nothing owed.
    pub fn new(ratio: R) -> Self {
        Self {
            ratio,
            passed_to: 0,
            credit: 0,
            lead: 0,
        }
    }

    /// The clock at master time `master` with nothing owed, its credit
    /// that of a clock passed there from master time 0: what a machine
    /// whose sub-processor was in step with the master clock since its
    /// power-on holds, so a machine reset or loaded there needs no credit
    /// of its own.
    pub fn at(ratio: R, master: u64) -> Self {
        Self {
            ratio,
            passed_to: master,
            credit: ratio.credit_at(master),
            lead: 0,
        }
    }

    /// The clock a savestate kept: the master time it was passed to, its
    /// credit and its lead. A credit at or past the denominator (a state
    /// of another region's ratio) is held below it.
    pub fn from_parts(ratio: R, passed_to: u64, credit: u32, lead: i64) -> Self {
        debug_assert!(
            credit < ratio.den(),
            "credit ({credit}) must be below the ratio's denominator ({}); the state may be another region's",
            ratio.den()
        );
        Self {
            ratio,
            passed_to,
            credit: credit.min(ratio.den().saturating_sub(1)),
            lead,
        }
    }

    /// The clock with its sub-processor standing `lead` cycles past the
    /// present (below 0, owing them).
    pub fn with_lead(self, lead: i64) -> Self {
        Self { lead, ..self }
    }

    /// Master time passes to `master`: the credit takes the span's share,
    /// and its whole cycles come due, owed by the sub-processor (its lead
    /// falls by them). Returns them, what a chip clocked at the
    /// sub-processor's rate is given for the span.
    #[inline]
    pub fn pass_to(&mut self, master: u64) -> u64 {
        debug_assert!(
            master >= self.passed_to,
            "master time {master} is before the {} the clock was passed to",
            self.passed_to
        );
        let (whole, credit) = self.ratio.split(master - self.passed_to, self.credit);
        self.passed_to = master;
        self.credit = credit;
        self.lead -= whole as i64;
        whole
    }

    /// The sub-processor is held (in reset, or off its bus): it runs
    /// nothing, so it owes nothing of the time that passed; an instruction
    /// it ran past the present before it was held still ends there.
    #[inline]
    pub fn hold(&mut self) {
        self.lead = self.lead.max(0);
    }

    /// The sub-processor ran `cycles`.
    #[inline]
    pub fn ran(&mut self, cycles: u32) {
        self.lead += i64::from(cycles);
    }

    /// Whether the sub-processor is behind the present.
    #[inline]
    pub fn owes(&self) -> bool {
        self.lead < 0
    }

    /// Whether the sub-processor owes more than `cycles`.
    #[inline]
    pub fn owes_more_than(&self, cycles: u32) -> bool {
        self.lead < -i64::from(cycles)
    }

    /// The master time the clock was passed to.
    #[inline]
    pub fn passed_to(&self) -> u64 {
        self.passed_to
    }

    /// The ratio's remainder, below its denominator.
    #[inline]
    pub fn credit(&self) -> u32 {
        self.credit
    }

    /// The sub-processor's cycles past the present; below 0, owed.
    #[inline]
    pub fn lead(&self) -> i64 {
        self.lead
    }

    /// Where the sub-processor stands on the master clock: master cycles
    /// past the present, rounded down (below 0, behind it). A chip that
    /// follows the sub-processor's time is brought there.
    #[inline]
    pub fn master_lead(&self) -> i64 {
        self.ratio.master_lead(self.lead, self.credit)
    }

    /// The ratio the clock divides the master clock by.
    #[inline]
    pub fn ratio(&self) -> R {
        self.ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::IntegerRateConverter;

    type Sevenths = FixedRatio<7, 15>;

    /// A small deterministic sequence of spans.
    fn spans(seed: u32, count: usize, most: u32) -> Vec<u64> {
        let mut x = seed;
        (0..count)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                u64::from(x >> 8) % u64::from(most + 1)
            })
            .collect()
    }

    #[test]
    fn a_fixed_ratio_owes_its_share_of_master_time_without_drift() {
        let mut clock = SubClock::new(Sevenths::default());
        let mut master = 0u64;
        let mut whole = 0u64;
        for span in spans(1, 10_000, 500) {
            master += span;
            whole += clock.pass_to(master);
            assert_eq!(whole, master * 7 / 15);
            assert_eq!(u64::from(clock.credit()), master * 7 % 15);
            assert_eq!(clock.lead(), -(whole as i64));
        }
        assert_eq!(clock.passed_to(), master);
    }

    #[test]
    fn a_clock_at_a_master_time_holds_the_credit_of_one_passed_there() {
        for master in [0u64, 1, 14, 15, 34, 1_000_003, u64::from(u32::MAX) * 9] {
            let derived = SubClock::at(Sevenths::default(), master);
            let mut passed = SubClock::new(Sevenths::default());
            let mut at = 0u64;
            while at < master {
                at = (at + 100_000_000).min(master);
                passed.pass_to(at);
            }
            assert_eq!(derived.credit(), passed.credit(), "at {master}");
            assert_eq!(derived.passed_to(), master);
            assert_eq!(derived.lead(), 0);
        }
        let ratio = Ratio::new(1_024_000, 21_477_270);
        let derived = SubClock::at(ratio, 123_456_789);
        let mut passed = SubClock::new(ratio);
        passed.pass_to(123_456_789);
        assert_eq!(derived.credit(), passed.credit());
    }

    #[test]
    fn a_ratio_is_kept_in_lowest_terms() {
        let ntsc = Ratio::new(1_024_000, 21_477_270);
        assert_eq!((ntsc.num(), ntsc.den()), (102_400, 2_147_727));
        let pal = Ratio::new(1_024_000, 21_281_370);
        assert_eq!((pal.num(), pal.den()), (102_400, 2_128_137));
        assert_eq!(
            (Sevenths::default().num(), Sevenths::default().den()),
            (7, 15)
        );
    }

    #[test]
    #[should_panic(expected = "above 0")]
    fn a_ratio_of_zero_is_refused() {
        Ratio::new(0, 15);
    }

    #[test]
    #[should_panic(expected = "fit 32 bits")]
    fn a_ratio_past_32_bits_is_refused() {
        Ratio::new(1, u64::from(u32::MAX) + 2);
    }

    #[test]
    fn a_ratio_counts_as_the_integer_rate_converter_does() {
        // The cycles due by each master time, the converter's count, are
        // what the clock has owed since it started.
        for master_hz in [21_477_270u64, 21_281_370] {
            let mut converter = IntegerRateConverter::new(1_024_000, master_hz);
            let mut clock = SubClock::new(Ratio::new(1_024_000, master_hz));
            let mut master = 0u64;
            let mut owed = 0u64;
            for span in spans(7, 2_000, 400_000) {
                master += span;
                let due = converter.cycles_to_samples(master);
                converter.advance(master, due);
                owed += clock.pass_to(master);
                assert_eq!(owed, due);
                assert_eq!(u64::from(clock.credit()), converter.remainder());
                assert_eq!(clock.passed_to(), converter.reference_cycles());
            }
        }
    }

    #[test]
    fn the_sub_processor_runs_what_it_owes_and_keeps_its_overshoot() {
        let mut clock = SubClock::new(Sevenths::default());
        assert_eq!(clock.pass_to(30), 14);
        assert!(clock.owes() && clock.owes_more_than(13) && !clock.owes_more_than(14));
        // Instructions of 4 cycles run while it owes: the last ends 2 past.
        while clock.owes() {
            clock.ran(4);
        }
        assert_eq!(clock.lead(), 2);
        // The next 2 cycles of its time are paid by that overshoot.
        assert_eq!(clock.pass_to(34), 1);
        assert_eq!(clock.lead(), 1);
        assert!(!clock.owes());
    }

    #[test]
    fn a_held_sub_processor_owes_nothing_and_keeps_a_lead_it_ran() {
        let mut clock = SubClock::new(Sevenths::default());
        clock.ran(5);
        clock.pass_to(15);
        clock.hold();
        assert_eq!(clock.lead(), 0);
        clock.pass_to(60);
        clock.hold();
        assert_eq!(clock.lead(), 0);
        let mut ahead = SubClock::new(Sevenths::default());
        ahead.ran(9);
        ahead.pass_to(15);
        ahead.hold();
        assert_eq!(
            ahead.lead(),
            2,
            "the instruction past the present still ends there"
        );
    }

    #[test]
    fn the_place_on_the_master_clock_is_the_lead_less_the_credit() {
        // At master time 34 the credit is 238 modulo 15, 13 master clocks
        // of the next Z80 cycle: a Z80 at its due cycle stands 13 master
        // clocks behind the present, 2 68000 cycles rounded down.
        let at = SubClock::at(Sevenths::default(), 34);
        assert_eq!(at.credit(), 13);
        assert_eq!(at.master_lead(), -2);
        assert_eq!(at.with_lead(2).master_lead(), 2);
        assert_eq!(at.with_lead(-3).master_lead(), -9);
        // On any clock, the place in master cycles is the floor of where
        // the sub-processor stands, counted from master time 0, less the
        // present.
        for ratio in [
            Ratio::new(7, 15),
            Ratio::new(1_024_000, 21_477_270),
            Ratio::new(3, 2),
        ] {
            let mut clock = SubClock::new(ratio);
            let mut master = 0u64;
            let mut owed = 0i64;
            for (step, span) in spans(3, 500, 2_000).into_iter().enumerate() {
                master += span;
                owed += clock.pass_to(master) as i64;
                if step % 3 == 0 {
                    clock.ran(5);
                }
                // The sub-processor has run `owed + lead` cycles: in master
                // cycles, `(owed + lead) * den / num`.
                let (num, den) = (i64::from(ratio.num()), i64::from(ratio.den()));
                let ran = owed + clock.lead();
                let place = (ran * den).div_euclid(num) - master as i64;
                assert_eq!(clock.master_lead(), place, "at {master}");
            }
        }
    }

    #[test]
    fn a_kept_clock_goes_on_as_the_one_it_was_taken_from() {
        let ratio = Ratio::new(1_024_000, 21_477_270);
        let mut clock = SubClock::new(ratio);
        clock.pass_to(1_000_001);
        clock.ran(70);
        let kept = SubClock::from_parts(ratio, clock.passed_to(), clock.credit(), clock.lead());
        assert_eq!(kept, clock);
        assert_eq!(
            SubClock::at(Sevenths::default(), 34).with_lead(-3).lead(),
            -3
        );
    }
}
