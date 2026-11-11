//! `compare-trace`: run the core live and diff its trace against a MAME
//! reference log, instruction by instruction.
//!
//! The reference file is streamed line-by-line through a large
//! `BufReader` (never loaded whole, a Genesis MAME log is millions of
//! lines), and each traced instruction is matched 1:1 against the next
//! parseable reference line. Divergence is decided on the architectural
//! registers only (PC, SR, D0-D7, A0-A7 for the 68000; PC + pairs +
//! interrupt state for the Z80); the MAME timing columns (`TC`/`FR`/`BX`/
//! `BY`) never trigger a divergence, and `--skip-timing` only tallies how
//! many register-clean lines drifted on timing.
//!
//! ## Wait-loop resync (`--resync N`)
//!
//! A naive 1:1 differ stalls on the first **polling loop** whose
//! iteration count legitimately differs between emulators (e.g. a
//! VBlank/HBlank wait or a Z80-handshake spin), both sides run the same
//! code and converge again the instant the awaited event lands, but the
//! instruction-locked comparison sees a permanent divergence at the loop.
//!
//! `--resync N` makes the differ tolerant of these phase differences: on
//! a mismatch it tries, within a window of `N` lines, to realign by
//! skipping the extra loop iterations on whichever side is lagging
//! (matching on the full non-ignored register state, so distinct loops,
//! which carry different registers, never false-match). A genuine
//! control-flow divergence still surfaces once the window is exhausted.
//! `--resync 0` (default) keeps the exact 1:1 behaviour.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::ops::ControlFlow;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use sg1_core::GenesisEmulator;

use crate::trace_driver::{drive_trace, RunLimit, TraceRun};
use crate::trace_ref::{self, TraceCpu, TraceLine};

/// Cap on how many individual `[resync]` notes are printed before they
/// are suppressed (the summary still reports the totals).
const RESYNC_NOTE_CAP: u64 = 100;

#[derive(Args, Debug)]
pub struct CompareTraceArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Path to the MAME reference trace log.
    pub reference: PathBuf,
    /// Which CPU to compare: `m68k` (68000) or `z80`.
    #[arg(long, default_value = "m68k")]
    pub cpu: String,
    /// Step N instructions untraced before comparing (skip the boot
    /// prologue to align with a reference captured mid-run).
    #[arg(long, default_value_t = 0)]
    pub warmup: u64,
    /// Tally (don't trigger) divergences where only the timing columns
    /// differ but every register matches.
    #[arg(long)]
    pub skip_timing: bool,
    /// Stop after this many divergences.
    #[arg(short = 'e', long, default_value_t = 1)]
    pub max_errors: usize,
    /// Comma-separated register names to ignore (e.g. `PC,SR,A7`).
    #[arg(short = 'g', long)]
    pub ignore: Option<String>,
    /// Cap the comparison at N frames.
    #[arg(short, long)]
    pub frames: Option<u64>,
    /// Cap the comparison at N instructions (68000 only).
    #[arg(short, long)]
    pub instructions: Option<u64>,
    /// Show a progress spinner.
    #[arg(long)]
    pub progress: bool,
    /// Write the report here instead of stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Tolerate wait-loop phase differences by realigning within a window
    /// of N lines. `0` (default) keeps the exact 1:1 comparison.
    #[arg(long, default_value_t = 0)]
    pub resync: usize,
    /// Frame-scheduled player-1 input `"F:buttons;F:buttons"`.
    #[arg(long)]
    pub press_at: Option<String>,
    /// Connect 6-button pads (the default is 3-button, matching MAME).
    #[arg(long)]
    pub six_button: bool,
}

/// One parseable reference line, buffered for resync lookahead.
struct RefEntry {
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
/// optional wait-loop resync.
struct Comparator<R: BufRead> {
    cpu: TraceCpu,
    reader: R,
    out: Box<dyn Write>,
    skip_timing: bool,
    ignore: HashSet<String>,
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
    /// Pull the next *parseable* reference line, advancing `ref_seq` past
    /// any stray lines (blank lines, interrupt notifications, etc.).
    fn pull_ref(&mut self) -> Result<Option<RefEntry>> {
        loop {
            let mut raw = String::new();
            let n = self.reader.read_line(&mut raw)?;
            if n == 0 {
                return Ok(None);
            }
            self.ref_seq += 1;
            let trimmed = raw.trim_end();
            if let Some(parsed) = TraceLine::parse(self.cpu, trimmed) {
                return Ok(Some(RefEntry {
                    raw: trimmed.to_string(),
                    parsed,
                    seq: self.ref_seq,
                }));
            }
        }
    }

    /// Ensure the lookahead buffer holds more than `idx` entries (or EOF
    /// is reached).
    fn ensure_ref(&mut self, idx: usize) -> Result<()> {
        while self.ref_buf.len() <= idx && !self.ref_eof {
            match self.pull_ref()? {
                Some(e) => self.ref_buf.push_back(e),
                None => self.ref_eof = true,
            }
        }
        Ok(())
    }

    /// Divergence is register-based: two lines match when no non-ignored
    /// register differs (timing columns never participate).
    fn is_match(&self, ours: &TraceLine, theirs: &TraceLine) -> bool {
        !ours.regs_differ(theirs, &self.ignore)
    }

    /// Pop the front reference line as an aligned match, updating the
    /// compared count and (under `--skip-timing`) the timing tally.
    fn consume_front_as_match(&mut self, ours: &TraceLine) {
        let front = self.ref_buf.pop_front().expect("front exists");
        self.compared += 1;
        if self.skip_timing && ours.only_timing_differs(&front.parsed, &self.ignore) {
            self.timing_skipped += 1;
        }
    }

    fn note_resync(&mut self, pc: u32, ours_extra: usize, ref_extra: usize) -> Result<()> {
        self.resync_events += 1;
        if self.resync_events <= RESYNC_NOTE_CAP {
            writeln!(
                self.out,
                "  [resync] @ instr {} pc {:06X}: skipped {} ours / {} ref line(s)",
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

    /// Emit a divergence report for the front reference line (exact path,
    /// `--resync 0`), consuming that line.
    fn report_front_mismatch(
        &mut self,
        our_line: &str,
        ours: &TraceLine,
    ) -> Result<ControlFlow<()>> {
        let front = self.ref_buf.pop_front().expect("front exists");
        self.compared += 1;
        let diffs = ours.register_diffs(&front.parsed, &self.ignore);
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
                "    {:<8} ours={:<12} ref={}",
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
        let ours = match TraceLine::parse(self.cpu, our_line) {
            Some(p) => p,
            None => return Ok(ControlFlow::Continue(())),
        };

        // Resync hold: our side spun extra loop iterations; wait for it to
        // catch up to the (fixed) front reference line.
        if self.holding {
            self.ensure_ref(0)?;
            let Some(front) = self.ref_buf.front() else {
                self.ref_eof = true;
                return Ok(ControlFlow::Break(()));
            };
            if self.is_match(&ours, &front.parsed) {
                let extra = self.pending_ours;
                self.ours_skipped += extra as u64;
                self.note_resync(ours.pc(), extra, 0)?;
                self.consume_front_as_match(&ours);
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
            self.consume_front_as_match(&ours);
            return Ok(ControlFlow::Continue(()));
        }

        // Mismatch. Without a resync budget, report it immediately.
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
            self.note_resync(ours.pc(), 0, k)?;
            self.consume_front_as_match(&ours);
            return Ok(ControlFlow::Continue(()));
        }

        // Otherwise our side looped more: hold the front reference line and
        // let subsequent instructions catch up. Remember this first
        // mismatch in case the window is exhausted (a real divergence).
        let front = self.ref_buf.front().expect("front exists");
        self.held = Some(HeldDivergence {
            ours_line: our_line.to_string(),
            ref_raw: front.raw.clone(),
            ref_seq: front.seq,
            diffs: ours.register_diffs(&front.parsed, &self.ignore),
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

pub fn run(args: CompareTraceArgs) -> Result<()> {
    let cpu = TraceCpu::parse(&args.cpu)?;
    if args.frames.is_some() && args.instructions.is_some() {
        anyhow::bail!("--frames and --instructions are mutually exclusive");
    }
    // No explicit cap == compare until the reference is exhausted (or the
    // divergence cap trips first).
    let limit = match (args.frames, args.instructions) {
        (_, Some(n)) => RunLimit::Instructions(n),
        (Some(n), _) => RunLimit::Frames(n),
        (None, None) => RunLimit::Unbounded,
    };

    let ignore = super::parse_ignore(args.ignore.as_deref());
    let skip_timing = args.skip_timing;
    let max_errors = args.max_errors.max(1);

    let (_, mut emu) = super::load_rom(&args.rom)?;
    // `load_rom` auto-selects from the ROM header; `--six-button` forces
    // 6-button pads when the tag is missing.
    if args.six_button {
        super::connect_controllers(&mut emu, true);
    }
    super::print_rom_title(&emu);

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
        writeln!(out, "skip-timing: ignoring the TC/FR/BX/BY columns")?;
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
        TraceRun {
            cpu,
            limit,
            warmup: args.warmup,
            with_timing: true,
        },
        progress.as_ref(),
        |emu: &mut GenesisEmulator, frame: u64| {
            super::apply_press(emu, schedule.as_ref(), frame);
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
    use std::io::Cursor;

    /// Build a parseable 68000 trace line. `d0` varies per loop iteration
    /// so distinct iterations carry distinct register state (and never
    /// false-match during resync).
    fn tl(pc: u32, disasm: &str, d0: u32) -> String {
        format!(
            "PC:{pc:06X} SR:0000 D0:{d0:08X} D1:00000000 D2:00000000 D3:00000000 D4:00000000 \
             D5:00000000 D6:00000000 D7:00000000 A0:00000000 A1:00000000 A2:00000000 A3:00000000 \
             A4:00000000 A5:00000000 A6:00000000 A7:00000000 | {disasm}"
        )
    }

    fn make_cmp(ref_text: &str, budget: usize) -> Comparator<Cursor<Vec<u8>>> {
        Comparator {
            cpu: TraceCpu::M68k,
            reader: Cursor::new(ref_text.as_bytes().to_vec()),
            out: Box::new(Vec::new()),
            skip_timing: false,
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
        let ref_text = format!("{}\n{}\n", tl(0x400, "NOP", 0), tl(0x402, "NOP", 0));
        let mut c = make_cmp(&ref_text, 0);
        assert!(matches!(
            c.feed(&tl(0x400, "NOP", 0)).unwrap(),
            ControlFlow::Continue(())
        ));
        assert!(matches!(
            c.feed(&tl(0x402, "NOP", 0)).unwrap(),
            ControlFlow::Continue(())
        ));
        assert_eq!(c.divergences, 0);
        assert_eq!(c.compared, 2);
        assert_eq!(c.resync_events, 0);
    }

    #[test]
    fn ours_looping_more_resyncs_by_skipping_ours() {
        // Reference already exited the loop (front = post-loop `addq`);
        // ours spins two extra iterations before catching up.
        let ref_text = format!("{}\n", tl(0x408, "ADDQ", 0x0080));
        let mut c = make_cmp(&ref_text, 8);
        assert!(matches!(
            c.feed(&tl(0x400, "BTST", 0x0042)).unwrap(),
            ControlFlow::Continue(())
        ));
        assert!(matches!(
            c.feed(&tl(0x404, "BEQ", 0x0042)).unwrap(),
            ControlFlow::Continue(())
        ));
        let cf = c.feed(&tl(0x408, "ADDQ", 0x0080)).unwrap();
        assert!(matches!(cf, ControlFlow::Continue(())));
        assert_eq!(c.divergences, 0);
        assert_eq!(c.resync_events, 1);
        assert_eq!(c.ours_skipped, 2);
        assert_eq!(c.ref_skipped, 0);
    }

    #[test]
    fn ref_looping_more_resyncs_by_skipping_ref() {
        // Ours exited the loop; the reference still has two spin lines
        // ahead of the matching post-loop `addq`.
        let ref_text = format!(
            "{}\n{}\n{}\n",
            tl(0x400, "BTST", 0x0042),
            tl(0x404, "BEQ", 0x0042),
            tl(0x408, "ADDQ", 0x0080)
        );
        let mut c = make_cmp(&ref_text, 8);
        let cf = c.feed(&tl(0x408, "ADDQ", 0x0080)).unwrap();
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
            tl(0x408, "ADDQ", 0x0080),
            tl(0x40A, "NOP", 0),
            tl(0x40C, "NOP", 0),
            tl(0x40E, "NOP", 0),
            tl(0x410, "NOP", 0)
        );
        let mut c = make_cmp(&ref_text, 3);
        for line in [
            tl(0x9000, "JMP", 0x00FF),
            tl(0x9006, "NOP", 0),
            tl(0x900C, "NOP", 0),
        ] {
            assert!(matches!(c.feed(&line).unwrap(), ControlFlow::Continue(())));
        }
        let cf = c.feed(&tl(0x9012, "NOP", 0)).unwrap();
        assert!(matches!(cf, ControlFlow::Break(())));
        assert_eq!(c.divergences, 1);
        // The held divergence is the first (branch-point) mismatch.
        assert_eq!(c.first_div_seq, Some(1));
    }

    #[test]
    fn resync_zero_reports_first_mismatch_immediately() {
        let ref_text = format!("{}\n", tl(0x408, "ADDQ", 0x0080));
        let mut c = make_cmp(&ref_text, 0);
        let cf = c.feed(&tl(0x9000, "JMP", 0x00FF)).unwrap();
        assert!(matches!(cf, ControlFlow::Break(())));
        assert_eq!(c.divergences, 1);
        assert_eq!(c.resync_events, 0);
    }

    #[test]
    fn timing_columns_never_trigger_divergence() {
        // Identical registers, different TC/BY: a match, not a divergence.
        let ours = "TC:0000000000000010 BY:0000 PC:000400 SR:0000 D0:00000000 D1:00000000 \
                    D2:00000000 D3:00000000 D4:00000000 D5:00000000 D6:00000000 D7:00000000 \
                    A0:00000000 A1:00000000 A2:00000000 A3:00000000 A4:00000000 A5:00000000 \
                    A6:00000000 A7:00000000 | NOP";
        let ref_text = "TC:0000000000000099 BY:00E0 PC:000400 SR:0000 D0:00000000 D1:00000000 \
                        D2:00000000 D3:00000000 D4:00000000 D5:00000000 D6:00000000 D7:00000000 \
                        A0:00000000 A1:00000000 A2:00000000 A3:00000000 A4:00000000 A5:00000000 \
                        A6:00000000 A7:00000000 | NOP\n";
        let mut c = make_cmp(ref_text, 0);
        c.skip_timing = true;
        assert!(matches!(c.feed(ours).unwrap(), ControlFlow::Continue(())));
        assert_eq!(c.divergences, 0);
        assert_eq!(c.compared, 1);
        assert_eq!(c.timing_skipped, 1);
    }
}
