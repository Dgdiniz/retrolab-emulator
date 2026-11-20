//! Comparing a trace with a reference: the live comparison `compare-trace`
//! makes as the machine runs, and the comparison of two trace files that
//! `compare` makes, both over [`TraceLine::differences`].
//!
//! ## Wait-loop resync (`--resync N`)
//!
//! A comparison one line against one line stalls on the first polling loop
//! whose iteration count legitimately differs between two emulators (a wait
//! for the vertical blank, a handshake with the sound CPU): both run the
//! same code and meet again the moment the awaited event lands, but the
//! comparison sees a divergence at the loop and nothing past it.
//!
//! With a window of `N` lines the comparator realigns instead: on a
//! mismatch it looks for our line within the next `N` reference lines (the
//! reference looped more, and its extra iterations are skipped), and
//! otherwise holds the reference line while our side catches up (ours
//! looped more). Lines match on every field the comparison looks at, so two
//! distinct loops, whose registers differ, never match. A real divergence
//! still shows once the window is spent: the first mismatch is reported.
//! `--resync 0` keeps the exact comparison.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::ops::ControlFlow;
use std::path::Path;

use anyhow::{Context, Result};
use indicatif::ProgressBar;

use super::line::{FieldDiff, TraceLine};

/// How many `[resync]` notes are printed before they are suppressed (the
/// summary still gives the totals).
pub const RESYNC_NOTE_CAP: u64 = 100;

/// What a comparison looks at.
#[derive(Clone, Debug)]
pub struct CompareRules {
    /// The CPU's line parser.
    pub parse: fn(&str) -> Option<TraceLine>,
    /// Leave the timing columns out, and count the lines whose fields match
    /// but whose timing differs.
    pub skip_timing: bool,
    /// The fields left out (upper-cased names).
    pub ignore: HashSet<String>,
    /// Stop after this many divergences.
    pub max_errors: usize,
}

impl CompareRules {
    fn matches(&self, ours: &TraceLine, theirs: &TraceLine) -> bool {
        !ours.differs_from(theirs, self.skip_timing, &self.ignore)
    }

    /// Whether a matching pair differs only in its timing, the lines
    /// `--skip-timing` counts.
    fn timing_only(&self, ours: &TraceLine, theirs: &TraceLine) -> bool {
        self.skip_timing && ours.differs_from(theirs, false, &self.ignore)
    }
}

/// The counts a comparison ends with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompareStats {
    /// Lines compared.
    pub compared: u64,
    /// Matching lines whose timing differed (with `--skip-timing`).
    pub timing_skipped: u64,
    /// Divergences reported.
    pub divergences: usize,
    /// The reference line of the first divergence.
    pub first_divergence: Option<u64>,
    /// Realignments made.
    pub resync_events: u64,
    /// Our lines skipped by them.
    pub ours_skipped: u64,
    /// Reference lines skipped by them.
    pub ref_skipped: u64,
}

/// One parseable reference line, kept for the resync window.
struct RefEntry {
    raw: String,
    parsed: TraceLine,
    /// Its line number in the reference (counting the lines that do not
    /// parse).
    seq: u64,
}

/// The first mismatch of a resync attempt, reported as it was if the window
/// is spent without realigning.
struct HeldDivergence {
    ours_line: String,
    ref_raw: String,
    ref_seq: u64,
    diffs: Vec<FieldDiff>,
}

/// Compares our trace, fed one line at a time, with a reference read as it
/// is needed, and writes the report.
pub struct Comparator<R: BufRead, W: Write> {
    rules: CompareRules,
    reader: R,
    out: W,
    resync_budget: usize,
    ref_buf: VecDeque<RefEntry>,
    ref_seq: u64,
    ref_eof: bool,
    stats: CompareStats,
    holding: bool,
    pending_ours: usize,
    held: Option<HeldDivergence>,
}

impl<R: BufRead, W: Write> Comparator<R, W> {
    /// A comparator of the reference `reader` holds, reporting to `out`,
    /// with a resync window of `resync` lines (`0`: none).
    pub fn new(rules: CompareRules, reader: R, out: W, resync: usize) -> Self {
        Self {
            rules,
            reader,
            out,
            resync_budget: resync,
            ref_buf: VecDeque::new(),
            ref_seq: 0,
            ref_eof: false,
            stats: CompareStats::default(),
            holding: false,
            pending_ours: 0,
            held: None,
        }
    }

    /// The counts so far.
    pub fn stats(&self) -> CompareStats {
        self.stats
    }

    /// Whether the reference has run out.
    pub fn reference_exhausted(&self) -> bool {
        self.ref_eof
    }

    /// The report written so far.
    pub fn output(&self) -> &W {
        &self.out
    }

    /// The next reference line that parses, past the ones that do not
    /// (blank lines, interrupt notes).
    fn pull_ref(&mut self) -> Result<Option<RefEntry>> {
        loop {
            let mut raw = String::new();
            if self.reader.read_line(&mut raw)? == 0 {
                return Ok(None);
            }
            self.ref_seq += 1;
            let trimmed = raw.trim_end();
            if let Some(parsed) = (self.rules.parse)(trimmed) {
                return Ok(Some(RefEntry {
                    raw: trimmed.to_string(),
                    parsed,
                    seq: self.ref_seq,
                }));
            }
        }
    }

    /// Read reference lines until the window holds more than `idx`, or the
    /// reference ends.
    fn ensure_ref(&mut self, idx: usize) -> Result<()> {
        while self.ref_buf.len() <= idx && !self.ref_eof {
            match self.pull_ref()? {
                Some(entry) => self.ref_buf.push_back(entry),
                None => self.ref_eof = true,
            }
        }
        Ok(())
    }

    /// Take the front reference line as matching ours.
    fn consume_front_as_match(&mut self, ours: &TraceLine) {
        let front = self.ref_buf.pop_front().expect("front exists");
        self.stats.compared += 1;
        if self.rules.timing_only(ours, &front.parsed) {
            self.stats.timing_skipped += 1;
        }
    }

    fn note_resync(&mut self, pc: &str, ours_extra: usize, ref_extra: usize) -> Result<()> {
        self.stats.resync_events += 1;
        if self.stats.resync_events <= RESYNC_NOTE_CAP {
            writeln!(
                self.out,
                "  [resync] @ instr {} pc {pc}: skipped {ours_extra} ours / {ref_extra} ref line(s)",
                self.stats.compared
            )?;
        } else if self.stats.resync_events == RESYNC_NOTE_CAP + 1 {
            writeln!(
                self.out,
                "  [resync] ... further resync notes suppressed (see summary for totals)"
            )?;
        }
        Ok(())
    }

    /// Report the front reference line as a divergence (the exact path),
    /// taking it.
    fn report_front_mismatch(
        &mut self,
        our_line: &str,
        ours: &TraceLine,
    ) -> Result<ControlFlow<()>> {
        let front = self.ref_buf.pop_front().expect("front exists");
        self.stats.compared += 1;
        let diffs = ours.differences(&front.parsed, self.rules.skip_timing, &self.rules.ignore);
        self.emit_divergence(our_line, &front.raw, front.seq, &diffs)
    }

    /// Report the held divergence (the resync window is spent).
    fn report_held(&mut self) -> Result<ControlFlow<()>> {
        let held = self.held.take().expect("held exists");
        self.emit_divergence(&held.ours_line, &held.ref_raw, held.ref_seq, &held.diffs)
    }

    fn emit_divergence(
        &mut self,
        our_line: &str,
        ref_raw: &str,
        ref_seq: u64,
        diffs: &[FieldDiff],
    ) -> Result<ControlFlow<()>> {
        self.stats.divergences += 1;
        self.stats.first_divergence.get_or_insert(ref_seq);
        writeln!(self.out)?;
        writeln!(
            self.out,
            "--- Divergence #{} (instruction {}, ref line {ref_seq}) ---",
            self.stats.divergences, self.stats.compared
        )?;
        writeln!(self.out, "  Ours: {our_line}")?;
        writeln!(self.out, "  Ref:  {ref_raw}")?;
        for diff in diffs {
            writeln!(
                self.out,
                "    {:<8} ours={:<12} ref={}",
                format!("{}:", diff.field),
                diff.ours,
                diff.theirs
            )?;
        }
        if self.stats.divergences >= self.rules.max_errors {
            Ok(ControlFlow::Break(()))
        } else {
            Ok(ControlFlow::Continue(()))
        }
    }

    /// Compare one line of ours; `Break` once the comparison is over (the
    /// divergences reached the limit, or the reference ended).
    pub fn feed(&mut self, our_line: &str) -> Result<ControlFlow<()>> {
        let Some(ours) = (self.rules.parse)(our_line) else {
            return Ok(ControlFlow::Continue(()));
        };

        // Holding: our side looped more; wait for it to reach the held
        // reference line.
        if self.holding {
            self.ensure_ref(0)?;
            let Some(front) = self.ref_buf.front() else {
                self.ref_eof = true;
                return Ok(ControlFlow::Break(()));
            };
            if self.rules.matches(&ours, &front.parsed) {
                let extra = self.pending_ours;
                self.stats.ours_skipped += extra as u64;
                self.note_resync(&ours.pc_text(), extra, 0)?;
                self.consume_front_as_match(&ours);
                self.holding = false;
                self.pending_ours = 0;
                self.held = None;
                return Ok(ControlFlow::Continue(()));
            }
            self.pending_ours += 1;
            if self.pending_ours > self.resync_budget {
                let flow = self.report_held()?;
                self.holding = false;
                self.pending_ours = 0;
                return Ok(flow);
            }
            return Ok(ControlFlow::Continue(()));
        }

        // In step: compare with the next reference line.
        self.ensure_ref(0)?;
        let Some(front) = self.ref_buf.front() else {
            self.ref_eof = true;
            return Ok(ControlFlow::Break(()));
        };
        if self.rules.matches(&ours, &front.parsed) {
            self.consume_front_as_match(&ours);
            return Ok(ControlFlow::Continue(()));
        }

        // A mismatch. With no window, it is reported at once.
        if self.resync_budget == 0 {
            return self.report_front_mismatch(our_line, &ours);
        }

        // The reference looped more if our line is within the window: its
        // extra iterations are skipped.
        let mut found = None;
        for k in 1..=self.resync_budget {
            self.ensure_ref(k)?;
            if self.ref_buf.len() <= k {
                break;
            }
            if self.rules.matches(&ours, &self.ref_buf[k].parsed) {
                found = Some(k);
                break;
            }
        }
        if let Some(k) = found {
            self.ref_buf.drain(..k);
            self.stats.ref_skipped += k as u64;
            self.note_resync(&ours.pc_text(), 0, k)?;
            self.consume_front_as_match(&ours);
            return Ok(ControlFlow::Continue(()));
        }

        // Otherwise our side looped more: hold the reference line, keeping
        // this mismatch to report if the window is spent.
        let front = self.ref_buf.front().expect("front exists");
        self.held = Some(HeldDivergence {
            ours_line: our_line.to_string(),
            ref_raw: front.raw.clone(),
            ref_seq: front.seq,
            diffs: ours.differences(&front.parsed, self.rules.skip_timing, &self.rules.ignore),
        });
        self.holding = true;
        self.pending_ours = 1;
        Ok(ControlFlow::Continue(()))
    }

    /// Write the summary, after `frames` frames of the run.
    pub fn finish(&mut self, frames: u64) -> Result<()> {
        let stats = self.stats;
        writeln!(self.out)?;
        writeln!(
            self.out,
            "Compared {} instruction(s) over {frames} frame(s).",
            stats.compared
        )?;
        if self.rules.skip_timing && stats.timing_skipped > 0 {
            writeln!(
                self.out,
                "Timing-only divergences skipped: {}",
                stats.timing_skipped
            )?;
        }
        if self.resync_budget > 0 && stats.resync_events > 0 {
            writeln!(
                self.out,
                "Resync: {} event(s), skipped {} ours / {} ref line(s).",
                stats.resync_events, stats.ours_skipped, stats.ref_skipped
            )?;
        }
        if stats.divergences == 0 {
            if self.ref_eof {
                writeln!(
                    self.out,
                    "Result: MATCH - reference exhausted with no divergences."
                )?;
            } else {
                writeln!(
                    self.out,
                    "Result: MATCH - no divergences in the compared window."
                )?;
            }
        } else {
            writeln!(
                self.out,
                "Result: DIVERGED - {} divergence(s); first at reference line {}.",
                stats.divergences,
                stats.first_divergence.unwrap_or(0)
            )?;
        }
        self.out.flush()?;
        Ok(())
    }
}

/// How `compare` reads two trace files.
#[derive(Clone, Debug)]
pub struct FileCompare {
    /// What the CPU is called in the report.
    pub label: &'static str,
    /// What the comparison looks at.
    pub rules: CompareRules,
    /// Lines of the first file shown before a divergence.
    pub context: usize,
    /// Lines skipped at the start of each file.
    pub skip1: u64,
    pub skip2: u64,
    /// Lines compared at most, after the skips.
    pub limit: Option<u64>,
}

/// Compare two trace files line against line, writing the report to `out`.
pub fn compare_files(
    first: &Path,
    second: &Path,
    options: &FileCompare,
    out: &mut dyn Write,
    progress: Option<&ProgressBar>,
) -> Result<CompareStats> {
    let file1 = File::open(first)
        .with_context(|| format!("failed to open trace 1: {}", first.display()))?;
    let file2 = File::open(second)
        .with_context(|| format!("failed to open trace 2: {}", second.display()))?;
    let rules = &options.rules;

    writeln!(out, "=== Trace Comparison ({}) ===", options.label)?;
    writeln!(
        out,
        "File 1: {} ({} bytes)",
        first.display(),
        file1.metadata()?.len()
    )?;
    writeln!(
        out,
        "File 2: {} ({} bytes)",
        second.display(),
        file2.metadata()?.len()
    )?;
    writeln!(
        out,
        "Options: skip1={}, skip2={}, limit={}, max_errors={}",
        options.skip1,
        options.skip2,
        options
            .limit
            .map_or_else(|| "unlimited".to_string(), |limit| limit.to_string()),
        rules.max_errors
    )?;
    if !rules.ignore.is_empty() {
        writeln!(out, "Ignoring: {}", sorted_names(&rules.ignore))?;
    }
    if rules.skip_timing {
        writeln!(out, "Skip-timing mode: ignoring timing-only divergences")?;
    }
    writeln!(out)?;

    let mut reader1 = BufReader::with_capacity(256 * 1024, file1);
    let mut reader2 = BufReader::with_capacity(256 * 1024, file2);
    skip_lines(&mut reader1, options.skip1)?;
    skip_lines(&mut reader2, options.skip2)?;

    let mut stats = CompareStats::default();
    let mut line_num = 0u64;
    let mut context: VecDeque<String> = VecDeque::with_capacity(options.context);
    let limit = options.limit.unwrap_or(u64::MAX);
    let mut line1 = String::new();
    let mut line2 = String::new();

    while line_num < limit {
        line1.clear();
        line2.clear();
        let read1 = reader1.read_line(&mut line1)? > 0;
        let read2 = reader2.read_line(&mut line2)? > 0;
        match (read1, read2) {
            (true, true) => {}
            (false, false) => break,
            (true, false) => {
                writeln!(
                    out,
                    "File 2 ended early. File 1 has more data after line {}.",
                    line_num + options.skip1
                )?;
                break;
            }
            (false, true) => {
                writeln!(
                    out,
                    "File 1 ended early. File 2 has more data after line {}.",
                    line_num + options.skip2
                )?;
                break;
            }
        }
        line_num += 1;
        let l1 = line1.trim_end();
        let l2 = line2.trim_end();
        if options.context > 0 {
            if context.len() >= options.context {
                context.pop_front();
            }
            context.push_back(l1.to_string());
        }

        match ((rules.parse)(l1), (rules.parse)(l2)) {
            (Some(e1), Some(e2)) => {
                if rules.matches(&e1, &e2) {
                    if rules.timing_only(&e1, &e2) {
                        stats.timing_skipped += 1;
                    }
                } else {
                    let at = line_num + options.skip1;
                    stats.divergences += 1;
                    stats.first_divergence.get_or_insert(at);
                    writeln!(
                        out,
                        "--- Divergence #{} at line {at} ---",
                        stats.divergences
                    )?;
                    if context.len() > 1 {
                        writeln!(out, "Context (file 1):")?;
                        let shown = context.len() - 1;
                        for (i, text) in context.iter().take(shown).enumerate() {
                            writeln!(out, "  {:>10}: {text}", at - (shown - i) as u64)?;
                        }
                    }
                    writeln!(out)?;
                    writeln!(out, "  File 1: {l1}")?;
                    writeln!(out, "  File 2: {l2}")?;
                    writeln!(out)?;
                    writeln!(out, "  Differences:")?;
                    for diff in e1.differences(&e2, rules.skip_timing, &rules.ignore) {
                        writeln!(out, "    {}: {} vs {}", diff.field, diff.ours, diff.theirs)?;
                    }
                    writeln!(out)?;
                    if stats.divergences >= rules.max_errors {
                        break;
                    }
                }
            }
            (None, _) => writeln!(
                out,
                "Warning: could not parse line {} in file 1: {l1}",
                line_num + options.skip1
            )?,
            (_, None) => writeln!(
                out,
                "Warning: could not parse line {} in file 2: {l2}",
                line_num + options.skip2
            )?,
        }

        if let Some(bar) = progress {
            if line_num.is_multiple_of(100_000) {
                bar.set_position(line_num);
            }
        }
    }
    stats.compared = line_num;

    writeln!(out, "=== Summary ===")?;
    writeln!(out, "Compared {line_num} lines")?;
    if stats.timing_skipped > 0 {
        writeln!(
            out,
            "Timing-only divergences skipped: {}",
            stats.timing_skipped
        )?;
    }
    if stats.divergences == 0 {
        writeln!(out, "Result: MATCH - No register divergences found!")?;
    } else {
        writeln!(
            out,
            "Result: DIVERGED - Found {} divergence(s)",
            stats.divergences
        )?;
        if let Some(first) = stats.first_divergence {
            writeln!(out, "First divergence at line {first}")?;
        }
    }
    out.flush()?;
    Ok(stats)
}

/// The names of a set, sorted, comma-separated.
pub fn sorted_names(names: &HashSet<String>) -> String {
    let mut names: Vec<&str> = names.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.join(", ")
}

/// Skip `count` lines, or to the end.
fn skip_lines<R: BufRead>(reader: &mut R, count: u64) -> std::io::Result<()> {
    let mut buf = String::new();
    for _ in 0..count {
        buf.clear();
        if reader.read_line(&mut buf)? == 0 {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::line::{shown_hex, TraceField};
    use std::io::Cursor;

    /// `PC:A` lines: a program counter and one register, in hex.
    fn parse(line: &str) -> Option<TraceLine> {
        let (pc, a) = line.split_once(':')?;
        let pc = u64::from_str_radix(pc, 16).ok()?;
        let a = u64::from_str_radix(a, 16).ok()?;
        Some(TraceLine {
            pc: pc as u32,
            fields: vec![
                TraceField::number("PC", pc, shown_hex::<6>),
                TraceField::number("A", a, shown_hex::<0>),
            ],
        })
    }

    fn comparator(reference: &str, resync: usize) -> Comparator<Cursor<Vec<u8>>, Vec<u8>> {
        let rules = CompareRules {
            parse,
            skip_timing: false,
            ignore: HashSet::new(),
            max_errors: 1,
        };
        Comparator::new(
            rules,
            Cursor::new(reference.as_bytes().to_vec()),
            Vec::new(),
            resync,
        )
    }

    fn feed(c: &mut Comparator<Cursor<Vec<u8>>, Vec<u8>>, lines: &[&str]) -> ControlFlow<()> {
        let mut flow = ControlFlow::Continue(());
        for line in lines {
            flow = c.feed(line).unwrap();
        }
        flow
    }

    #[test]
    fn the_held_mismatch_is_reported_once_the_window_is_spent() {
        let mut c = comparator("8003:80\n8004:0\n8005:0\n", 2);
        assert!(feed(&mut c, &["9000:ff", "9003:0"]).is_continue());
        assert!(feed(&mut c, &["9006:0"]).is_break());
        let stats = c.stats();
        assert_eq!(stats.divergences, 1);
        assert_eq!(stats.first_divergence, Some(1));
        let report = String::from_utf8(c.output().clone()).unwrap();
        assert!(
            report.contains("--- Divergence #1 (instruction 0, ref line 1) ---"),
            "{report}"
        );
        assert!(
            report.contains("    PC:      ours=009000       ref=008003"),
            "{report}"
        );
    }

    #[test]
    fn a_resync_note_names_the_pc_as_its_field_shows_it() {
        let mut c = comparator("8000:42\n8002:42\n8003:80\n", 8);
        assert!(feed(&mut c, &["8003:80"]).is_continue());
        let stats = c.stats();
        assert_eq!(
            (stats.resync_events, stats.ref_skipped, stats.ours_skipped),
            (1, 2, 0)
        );
        let report = String::from_utf8(c.output().clone()).unwrap();
        assert_eq!(
            report,
            "  [resync] @ instr 0 pc 008003: skipped 0 ours / 2 ref line(s)\n"
        );
    }

    #[test]
    fn the_summary_says_how_the_comparison_ended() {
        let mut c = comparator("8000:1\n", 0);
        assert!(feed(&mut c, &["8000:1"]).is_continue());
        assert!(feed(&mut c, &["8002:1"]).is_break());
        assert!(c.reference_exhausted());
        c.finish(3).unwrap();
        let report = String::from_utf8(c.output().clone()).unwrap();
        assert!(report.ends_with(
            "\nCompared 1 instruction(s) over 3 frame(s).\nResult: MATCH - reference exhausted with no divergences.\n"
        ), "{report}");
    }

    #[test]
    fn two_files_are_compared_line_against_line() {
        let dir = std::env::temp_dir().join(format!("retro-cli-compare-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.txt");
        let second = dir.join("second.txt");
        std::fs::write(&first, "8000:1\n8002:2\n8004:3\n").unwrap();
        std::fs::write(&second, "8000:1\n8002:7\n").unwrap();
        let options = FileCompare {
            label: "test",
            rules: CompareRules {
                parse,
                skip_timing: false,
                ignore: HashSet::new(),
                max_errors: 5,
            },
            context: 3,
            skip1: 0,
            skip2: 0,
            limit: None,
        };
        let mut out = Vec::new();
        let stats = compare_files(&first, &second, &options, &mut out, None).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            (stats.compared, stats.divergences, stats.first_divergence),
            (2, 1, Some(2))
        );
        let report = String::from_utf8(out).unwrap();
        assert!(
            report.contains(
                "--- Divergence #1 at line 2 ---\nContext (file 1):\n           1: 8000:1\n"
            ),
            "{report}"
        );
        assert!(
            report.contains("  Differences:\n    A: 2 vs 7\n"),
            "{report}"
        );
        assert!(
            report.contains("File 2 ended early. File 1 has more data after line 2."),
            "{report}"
        );
        assert!(
            report.ends_with(
                "Result: DIVERGED - Found 1 divergence(s)\nFirst divergence at line 2\n"
            ),
            "{report}"
        );
    }
}
