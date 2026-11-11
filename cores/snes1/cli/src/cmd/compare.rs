//! `compare-trace`: run the core live and diff its trace against a
//! bsnes-plus reference log, instruction by instruction.
//!
//! The reference file is streamed line-by-line through a large
//! `BufReader` (never loaded whole: a 65C816 reference log can run to
//! millions of lines), and each of our traced instructions is matched
//! 1:1 against the next parseable reference line. The first `--max-errors`
//! divergences are reported with a per-field breakdown, then the run
//! stops.
//!
//! ## Wait-loop resync (`--resync N`)
//!
//! A naive 1:1 differ stalls on the first **polling loop** whose
//! iteration count legitimately differs between emulators, e.g. the
//! boot APU handshake (`cmp $2140 / bne`), where snes1's APU produces
//! each handshake byte a few instructions later than bsnes. Both sides
//! execute the *same* code and converge again the instant the awaited
//! event lands, but the instruction-locked comparison sees a permanent
//! divergence at the loop and reports nothing past it.
//!
//! `--resync N` makes the differ tolerant of these phase differences:
//! on a mismatch it tries, within a window of `N` lines, to realign by
//! skipping the extra loop iterations on whichever side is lagging
//! (matching on the full non-ignored register state, so distinct loops,
//! which carry different `A`/`X`, never false-match). A genuine
//! control-flow divergence still surfaces: the lagging side never
//! produces the leading side's line, the window is exhausted, and the
//! original branch-point mismatch is reported. `--resync 0` (default)
//! keeps the exact 1:1 behaviour.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::ops::ControlFlow;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use retro_core::{EmulatorCore, InputState};
use snes1_core::SnesEmulator;

use crate::trace_driver::{drive_trace, RunLimit};
use crate::trace_ref::{self, TraceCpu, TraceLine};

/// Cap on how many individual `[resync]` notes are printed before they
/// are suppressed (the summary still reports the totals). A handshake
/// or VBlank-poll heavy run can resync hundreds of times; the first
/// batch is enough to see the pattern.
const RESYNC_NOTE_CAP: u64 = 100;

#[derive(Args, Debug)]
pub struct CompareArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Path to the bsnes-plus reference trace log.
    pub reference: PathBuf,
    /// Which CPU to compare: `snes` (65C816), `spc700`, `gsu` (Super FX) or
    /// `cx4` (Cx4 HG51B169). The `gsu`/`cx4` modes self-diff against a saved
    /// log, there is no bsnes-plus GSU/Cx4 reference.
    #[arg(long, default_value = "snes")]
    pub cpu: String,
    /// Ignore timing-only divergences (the 65C816 `V`/`H`/`F` columns).
    #[arg(long)]
    pub skip_timing: bool,
    /// Stop after this many divergences.
    #[arg(long, default_value_t = 1)]
    pub max_errors: usize,
    /// Comma-separated register/field names to ignore (e.g. `A,X,DB,P,eff`).
    #[arg(long)]
    pub ignore: Option<String>,
    /// Cap the comparison at N frames.
    #[arg(long)]
    pub frames: Option<u64>,
    /// Cap the comparison at N instructions (65C816 only).
    #[arg(long)]
    pub instructions: Option<u64>,
    /// Show a progress spinner (on stderr).
    #[arg(long)]
    pub progress: bool,
    /// Write the report here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
    /// Tolerate wait-loop phase differences by realigning within a
    /// window of N lines (skipping the extra loop iterations on the
    /// lagging side). `0` (default) keeps the exact 1:1 comparison.
    /// Use a generous window (e.g. `512`) to get past the boot APU
    /// handshake and reach deep test code; a real control-flow
    /// divergence is still reported once the window is exhausted.
    #[arg(long, default_value_t = 0)]
    pub resync: usize,
    /// Frame-scheduled player-1 input: `"F:buttons;F:buttons"` (e.g.
    /// `"180:start;186:"`). Drives snes1 to the same screen the reference
    /// log captured (its presses were timed by hand). Each set-point
    /// holds its mask from frame `F` until the next; an empty list
    /// releases. Without it the compare runs with no input.
    #[arg(long)]
    pub press_at: Option<String>,
    /// Load low-confidence ROMs (blank title / bad checksum) by forcing
    /// the vector-implied mapping instead of failing the detector gate.
    #[arg(long)]
    pub force_detect: bool,
    /// DSP-1 firmware + coprocessor-mode flags (DSP-1 games need
    /// `--dsp1-firmware` to stay in lockstep with a bsnes-plus trace).
    #[command(flatten)]
    pub coprocessor: super::CoprocessorArgs,
}

/// One parseable reference line, buffered for resync lookahead.
struct RefEntry {
    /// The original line text, for the `Ref:` report row.
    raw: String,
    parsed: TraceLine,
    /// 1-based reference line number (counts unparseable lines too).
    seq: u64,
}

/// The first mismatch of a resync attempt, kept so it can be reported
/// verbatim if the window is exhausted without realigning.
struct HeldDivergence {
    ours_line: String,
    ref_raw: String,
    ref_seq: u64,
    diffs: Vec<trace_ref::FieldDiff>,
}

/// Streams the reference and diffs our live trace against it, with
/// optional wait-loop resync. Owns the reference reader and the report
/// sink so the `drive_trace` callback can delegate to `feed`.
struct Comparator<R: BufRead> {
    cpu: TraceCpu,
    reader: R,
    out: Box<dyn Write>,
    skip_timing: bool,
    ignore: std::collections::HashSet<String>,
    max_errors: usize,
    resync_budget: usize,

    // Reference stream + lookahead buffer.
    ref_buf: VecDeque<RefEntry>,
    ref_seq: u64,
    ref_eof: bool,

    // Stats.
    compared: u64,
    timing_skipped: u64,
    divergences: usize,
    first_div_seq: Option<u64>,

    // Resync state.
    holding: bool,
    pending_ours: usize,
    held: Option<HeldDivergence>,
    ref_skipped: u64,
    ours_skipped: u64,
    resync_events: u64,
}

impl<R: BufRead> Comparator<R> {
    /// Pull the next *parseable* reference line, advancing `ref_seq`
    /// past any stray lines. `None` at EOF.
    fn pull_ref(&mut self) -> Result<Option<RefEntry>> {
        loop {
            let mut raw = String::new();
            let n = self.reader.read_line(&mut raw)?;
            if n == 0 {
                return Ok(None);
            }
            self.ref_seq += 1;
            let trimmed = raw.trim_end();
            if let Some(parsed) = trace_ref::parse(self.cpu, trimmed) {
                return Ok(Some(RefEntry {
                    raw: trimmed.to_string(),
                    parsed,
                    seq: self.ref_seq,
                }));
            }
        }
    }

    /// Ensure the lookahead buffer holds more than `idx` entries (or
    /// EOF is reached).
    fn ensure_ref(&mut self, idx: usize) -> Result<()> {
        while self.ref_buf.len() <= idx && !self.ref_eof {
            match self.pull_ref()? {
                Some(e) => self.ref_buf.push_back(e),
                None => self.ref_eof = true,
            }
        }
        Ok(())
    }

    fn is_match(&self, ours: &TraceLine, theirs: &TraceLine) -> bool {
        ours.differs(theirs, self.skip_timing, &self.ignore)
            .is_empty()
    }

    /// Pop the front reference line as an aligned match, updating the
    /// compared count and the skipped-timing tally.
    fn consume_front_as_match(&mut self, ours: &TraceLine) -> Result<()> {
        let front = self.ref_buf.pop_front().expect("front exists");
        self.compared += 1;
        if self.skip_timing && !ours.differs(&front.parsed, false, &self.ignore).is_empty() {
            self.timing_skipped += 1;
        }
        Ok(())
    }

    fn note_resync(&mut self, pc: u32, ours_extra: usize, ref_extra: usize) -> Result<()> {
        self.resync_events += 1;
        if self.resync_events <= RESYNC_NOTE_CAP {
            writeln!(
                self.out,
                "  [resync] @ instr {} pc {:06x}: skipped {} ours / {} ref line(s)",
                self.compared, pc, ours_extra, ref_extra
            )?;
        } else if self.resync_events == RESYNC_NOTE_CAP + 1 {
            writeln!(
                self.out,
                "  [resync] ... further resync notes suppressed (see summary for totals)"
            )?;
        }
        Ok(())
    }

    /// Emit a divergence report for the front reference line (exact
    /// path, `--resync 0`), consuming that line.
    fn report_front_mismatch(
        &mut self,
        our_line: &str,
        ours: &TraceLine,
    ) -> Result<ControlFlow<()>> {
        let front = self.ref_buf.pop_front().expect("front exists");
        self.compared += 1;
        let diffs = ours.differs(&front.parsed, self.skip_timing, &self.ignore);
        self.emit_divergence(our_line, &front.raw, front.seq, &diffs)
    }

    /// Emit the held divergence (resync window exhausted).
    fn report_held(&mut self) -> Result<ControlFlow<()>> {
        let held = self.held.take().expect("held exists");
        self.emit_divergence(&held.ours_line, &held.ref_raw, held.ref_seq, &held.diffs)
    }

    fn emit_divergence(
        &mut self,
        our_line: &str,
        ref_raw: &str,
        ref_seq: u64,
        diffs: &[trace_ref::FieldDiff],
    ) -> Result<ControlFlow<()>> {
        self.divergences += 1;
        if self.first_div_seq.is_none() {
            self.first_div_seq = Some(ref_seq);
        }
        writeln!(self.out)?;
        writeln!(
            self.out,
            "--- Divergence #{} (instruction {}, ref line {}) ---",
            self.divergences, self.compared, ref_seq
        )?;
        writeln!(self.out, "  Ours: {our_line}")?;
        writeln!(self.out, "  Ref:  {ref_raw}")?;
        for d in diffs {
            writeln!(
                self.out,
                "    {:<8} ours={:<10} ref={}",
                format!("{}:", d.field),
                d.ours,
                d.theirs
            )?;
        }
        if self.divergences >= self.max_errors {
            Ok(ControlFlow::Break(()))
        } else {
            Ok(ControlFlow::Continue(()))
        }
    }

    fn feed(&mut self, our_line: &str) -> Result<ControlFlow<()>> {
        let ours = match trace_ref::parse(self.cpu, our_line) {
            Some(p) => p,
            None => return Ok(ControlFlow::Continue(())),
        };

        // Resync hold: our side spun extra loop iterations; wait for it
        // to catch up to the (fixed) front reference line.
        if self.holding {
            self.ensure_ref(0)?;
            let Some(front) = self.ref_buf.front() else {
                self.ref_eof = true;
                return Ok(ControlFlow::Break(()));
            };
            if self.is_match(&ours, &front.parsed) {
                let extra = self.pending_ours;
                self.ours_skipped += extra as u64;
                self.note_resync(ours.pc, extra, 0)?;
                self.consume_front_as_match(&ours)?;
                self.holding = false;
                self.pending_ours = 0;
                self.held = None;
                return Ok(ControlFlow::Continue(()));
            }
            self.pending_ours += 1;
            if self.pending_ours > self.resync_budget {
                let cf = self.report_held()?;
                self.holding = false;
                self.pending_ours = 0;
                return Ok(cf);
            }
            return Ok(ControlFlow::Continue(()));
        }

        // Synced: compare against the next reference line.
        self.ensure_ref(0)?;
        let Some(front) = self.ref_buf.front() else {
            self.ref_eof = true;
            return Ok(ControlFlow::Break(()));
        };
        if self.is_match(&ours, &front.parsed) {
            self.consume_front_as_match(&ours)?;
            return Ok(ControlFlow::Continue(()));
        }

        // Mismatch. Without a resync budget, report it immediately
        // (exact 1:1 behaviour).
        if self.resync_budget == 0 {
            return self.report_front_mismatch(our_line, &ours);
        }

        // Resync, first try the reference side: if our (post-loop) line
        // appears within the window, the reference looped more; drop its
        // extra iterations.
        let mut found_k = None;
        for k in 1..=self.resync_budget {
            self.ensure_ref(k)?;
            if self.ref_buf.len() <= k {
                break; // reference EOF inside the window
            }
            if self.is_match(&ours, &self.ref_buf[k].parsed) {
                found_k = Some(k);
                break;
            }
        }
        if let Some(k) = found_k {
            for _ in 0..k {
                self.ref_buf.pop_front();
            }
            self.ref_skipped += k as u64;
            self.note_resync(ours.pc, 0, k)?;
            self.consume_front_as_match(&ours)?;
            return Ok(ControlFlow::Continue(()));
        }

        // Otherwise our side looped more: hold the front reference line
        // and let subsequent instructions catch up. Remember this first
        // mismatch in case the window is exhausted (a real divergence).
        let front = self.ref_buf.front().expect("front exists");
        self.held = Some(HeldDivergence {
            ours_line: our_line.to_string(),
            ref_raw: front.raw.clone(),
            ref_seq: front.seq,
            diffs: ours.differs(&front.parsed, self.skip_timing, &self.ignore),
        });
        self.holding = true;
        self.pending_ours = 1;
        Ok(ControlFlow::Continue(()))
    }

    fn finish(&mut self, frames: u64) -> Result<()> {
        writeln!(self.out)?;
        writeln!(
            self.out,
            "Compared {} instruction(s) over {} frame(s).",
            self.compared, frames
        )?;
        if self.skip_timing && self.timing_skipped > 0 {
            writeln!(
                self.out,
                "Timing-only divergences skipped: {}",
                self.timing_skipped
            )?;
        }
        if self.resync_budget > 0 && self.resync_events > 0 {
            writeln!(
                self.out,
                "Resync: {} event(s), skipped {} ours / {} ref line(s).",
                self.resync_events, self.ours_skipped, self.ref_skipped
            )?;
        }
        if self.divergences == 0 {
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
                self.divergences,
                self.first_div_seq.unwrap_or(0)
            )?;
        }
        self.out.flush()?;
        Ok(())
    }
}

pub fn run(args: CompareArgs) -> Result<()> {
    let cpu = TraceCpu::parse(&args.cpu)?;
    if args.frames.is_some() && args.instructions.is_some() {
        anyhow::bail!("--frames and --instructions are mutually exclusive");
    }
    // No explicit cap == compare until the reference is exhausted (or
    // the divergence cap trips first).
    let limit = match (args.frames, args.instructions) {
        (_, Some(n)) => RunLimit::Instructions(n),
        (Some(n), _) => RunLimit::Frames(n),
        (None, None) => RunLimit::Unbounded,
    };

    let ignore = super::parse_ignore(args.ignore.as_deref());
    let skip_timing = args.skip_timing;
    let max_errors = args.max_errors.max(1);

    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    args.coprocessor.apply(&mut emu)?;
    super::apply_force_detect(&mut emu, &rom, args.force_detect);
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;
    if let Some(chip) = emu.coprocessor_label() {
        writeln!(std::io::stderr(), "coprocessor: {chip}").ok();
    }

    // Frame-scheduled controller input (reproduces the reference log's
    // timed human presses). Empty/absent -> no injection.
    let schedule = super::parse_press_schedule(args.press_at.as_deref())?;

    let ref_file = File::open(&args.reference)
        .with_context(|| format!("failed to open reference: {}", args.reference.display()))?;
    let reader = BufReader::with_capacity(256 * 1024, ref_file);

    let mut out = super::open_output(args.output.as_ref())?;
    writeln!(
        out,
        "Comparing {} trace of {} against {}",
        cpu.label(),
        args.rom.display(),
        args.reference.display()
    )?;
    if skip_timing {
        writeln!(out, "skip-timing: ignoring the V/H/F columns")?;
    }
    if !ignore.is_empty() {
        let mut names: Vec<&str> = ignore.iter().map(|s| s.as_str()).collect();
        names.sort_unstable();
        writeln!(out, "ignoring fields: {}", names.join(", "))?;
    }
    if args.resync > 0 {
        writeln!(out, "resync window: {} line(s)", args.resync)?;
    }

    let progress = make_spinner(args.progress, cpu.label());

    let mut cmp = Comparator {
        cpu,
        reader,
        out,
        skip_timing,
        ignore,
        max_errors,
        resync_budget: args.resync,
        ref_buf: VecDeque::new(),
        ref_seq: 0,
        ref_eof: false,
        compared: 0,
        timing_skipped: 0,
        divergences: 0,
        first_div_seq: None,
        holding: false,
        pending_ours: 0,
        held: None,
        ref_skipped: 0,
        ours_skipped: 0,
        resync_events: 0,
    };

    let stats = drive_trace(
        &mut emu,
        cpu,
        limit,
        progress.as_ref(),
        |emu: &mut SnesEmulator, frame: u64| {
            if let Some(sched) = &schedule {
                if let Some(buttons) = sched.mask_at(frame) {
                    emu.set_input(0, InputState { buttons });
                }
            }
        },
        |our_line| cmp.feed(our_line),
    )?;

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    cmp.finish(stats.frames)?;
    Ok(())
}

fn make_spinner(show: bool, label: &str) -> Option<ProgressBar> {
    if !show {
        return None;
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template(&format!(
                "{{spinner:.green}} comparing {label}: {{pos}} instructions"
            ))
            .unwrap(),
    );
    Some(pb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::Cursor;

    /// Build a parseable 65C816 trace line (timing columns fixed; only
    /// `pc`/`disasm`/`A` vary, which is all the resync logic keys on
    /// here since the tests run with `skip_timing`).
    fn tl(pc: u32, disasm: &str, a: u16) -> String {
        format!(
            "{pc:06x} {disasm:<22} A:{a:04x} X:0000 Y:0000 S:01ff D:0000 DB:00 ..M..I.C V:  0 H:  0 F: 0"
        )
    }

    fn make_cmp(ref_text: &str, budget: usize) -> Comparator<Cursor<Vec<u8>>> {
        Comparator {
            cpu: TraceCpu::Snes,
            reader: Cursor::new(ref_text.as_bytes().to_vec()),
            out: Box::new(Vec::new()),
            skip_timing: true,
            ignore: HashSet::new(),
            max_errors: 1,
            resync_budget: budget,
            ref_buf: VecDeque::new(),
            ref_seq: 0,
            ref_eof: false,
            compared: 0,
            timing_skipped: 0,
            divergences: 0,
            first_div_seq: None,
            holding: false,
            pending_ours: 0,
            held: None,
            ref_skipped: 0,
            ours_skipped: 0,
            resync_events: 0,
        }
    }

    #[test]
    fn exact_match_consumes_without_divergence() {
        let ref_text = format!("{}\n{}\n", tl(0x8000, "nop", 0), tl(0x8002, "nop", 0));
        let mut c = make_cmp(&ref_text, 0);
        assert!(matches!(
            c.feed(&tl(0x8000, "nop", 0)).unwrap(),
            ControlFlow::Continue(())
        ));
        assert!(matches!(
            c.feed(&tl(0x8002, "nop", 0)).unwrap(),
            ControlFlow::Continue(())
        ));
        assert_eq!(c.divergences, 0);
        assert_eq!(c.compared, 2);
        assert_eq!(c.resync_events, 0);
    }

    #[test]
    fn ours_looping_more_resyncs_by_skipping_ours() {
        // Reference already exited the loop (front = post-loop `inc`);
        // ours spins two extra iterations before catching up.
        let ref_text = format!("{}\n", tl(0x8003, "inc $20", 0x0080));
        let mut c = make_cmp(&ref_text, 8);
        assert!(matches!(
            c.feed(&tl(0x8000, "lda $4210", 0x0042)).unwrap(),
            ControlFlow::Continue(())
        ));
        assert!(matches!(
            c.feed(&tl(0x8002, "bpl $8000", 0x0042)).unwrap(),
            ControlFlow::Continue(())
        ));
        let cf = c.feed(&tl(0x8003, "inc $20", 0x0080)).unwrap();
        assert!(matches!(cf, ControlFlow::Continue(())));
        assert_eq!(c.divergences, 0);
        assert_eq!(c.resync_events, 1);
        assert_eq!(c.ours_skipped, 2);
        assert_eq!(c.ref_skipped, 0);
    }

    #[test]
    fn ref_looping_more_resyncs_by_skipping_ref() {
        // Ours exited the loop; the reference still has two spin lines
        // ahead of the matching post-loop `inc`.
        let ref_text = format!(
            "{}\n{}\n{}\n",
            tl(0x8000, "lda $4210", 0x0042),
            tl(0x8002, "bpl $8000", 0x0042),
            tl(0x8003, "inc $20", 0x0080)
        );
        let mut c = make_cmp(&ref_text, 8);
        let cf = c.feed(&tl(0x8003, "inc $20", 0x0080)).unwrap();
        assert!(matches!(cf, ControlFlow::Continue(())));
        assert_eq!(c.divergences, 0);
        assert_eq!(c.resync_events, 1);
        assert_eq!(c.ref_skipped, 2);
        assert_eq!(c.ours_skipped, 0);
    }

    #[test]
    fn real_divergence_reported_once_window_exhausted() {
        // Ours takes a genuinely different path; nothing in the window
        // realigns, so the original branch-point mismatch is reported.
        let ref_text = format!(
            "{}\n{}\n{}\n{}\n{}\n",
            tl(0x8003, "inc $20", 0x0080),
            tl(0x8004, "nop", 0),
            tl(0x8005, "nop", 0),
            tl(0x8006, "nop", 0),
            tl(0x8007, "nop", 0)
        );
        let mut c = make_cmp(&ref_text, 3);
        for line in [
            tl(0x9000, "jmp $9000", 0x00ff),
            tl(0x9003, "nop", 0),
            tl(0x9006, "nop", 0),
        ] {
            assert!(matches!(c.feed(&line).unwrap(), ControlFlow::Continue(())));
        }
        let cf = c.feed(&tl(0x9009, "nop", 0)).unwrap();
        assert!(matches!(cf, ControlFlow::Break(())));
        assert_eq!(c.divergences, 1);
        // The held divergence is the first (branch-point) mismatch.
        assert_eq!(c.first_div_seq, Some(1));
    }

    #[test]
    fn resync_zero_reports_first_mismatch_immediately() {
        let ref_text = format!("{}\n", tl(0x8003, "inc $20", 0x0080));
        let mut c = make_cmp(&ref_text, 0);
        let cf = c.feed(&tl(0x9000, "jmp $9000", 0x00ff)).unwrap();
        assert!(matches!(cf, ControlFlow::Break(())));
        assert_eq!(c.divergences, 1);
        assert_eq!(c.resync_events, 0);
    }
}
