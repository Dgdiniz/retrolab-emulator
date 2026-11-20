//! `compare-trace`: run the machine live and diff its trace against a
//! reference log, instruction by instruction. The reference is read as it
//! is needed (a log can run to millions of lines), and each of our
//! instructions is matched against its next line that parses; the first
//! `--max-errors` divergences are reported field by field, then the run
//! stops. `--resync` realigns past wait loops whose iteration counts differ
//! ([`crate::trace::compare`]).

use std::fs::File;
use std::io::{BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::console::{find_cpu, Console};
use crate::input::InputArgs;
use crate::output::open_output;
use crate::progress;
use crate::rom::{load, write_banner, RomArgs};
use crate::trace::compare::{sorted_names, Comparator, CompareRules};
use crate::trace::line::parse_ignore;
use crate::trace::{check_cpu, drive_trace, RunLimit, TraceRun};

#[derive(clap::Args, Debug)]
pub struct CompareTraceArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Path to the reference trace log.
    pub reference: PathBuf,
    /// Which CPU to compare.
    #[arg(long, default_value = "")]
    pub cpu: String,
    /// Step N instructions of the main CPU untraced before comparing (skip
    /// the boot prologue to align with a reference captured mid-run).
    #[arg(long, default_value_t = 0)]
    pub warmup: u64,
    /// Leave the timing columns out of the comparison, counting the lines
    /// whose registers match while their timing differs.
    #[arg(long)]
    pub skip_timing: bool,
    /// Stop after this many divergences.
    #[arg(short = 'e', long, default_value_t = 1)]
    pub max_errors: usize,
    /// Comma-separated register or field names to ignore.
    #[arg(short = 'g', long)]
    pub ignore: Option<String>,
    /// Cap the comparison at N frames.
    #[arg(short, long)]
    pub frames: Option<u64>,
    /// Cap the comparison at N instructions of the main CPU.
    #[arg(short, long)]
    pub instructions: Option<u64>,
    /// Show a progress spinner (on stderr).
    #[arg(long)]
    pub progress: bool,
    /// Write the report here instead of stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Tolerate wait-loop phase differences by realigning within a window of
    /// N lines (skipping the extra loop iterations on the lagging side).
    /// `0` (default) keeps the exact 1:1 comparison. Use a generous window
    /// (e.g. `512`) to get past a boot handshake and reach the code after it;
    /// a real control-flow divergence is still reported once the window is
    /// exhausted.
    #[arg(long, default_value_t = 0)]
    pub resync: usize,
    #[command(flatten)]
    pub input: InputArgs,
}

pub fn run<C: Console>(args: CompareTraceArgs<C::BootArgs>) -> Result<()> {
    let cpu = find_cpu::<C>(&args.cpu)?;
    // No limit: compare until the reference ends, or the divergences reach
    // the limit.
    let limit =
        RunLimit::from_flags(args.frames, args.instructions)?.unwrap_or(RunLimit::Unbounded);
    cpu.check_available()?;
    let input = args.input.plan::<C::Emulator>()?;
    let ignore = parse_ignore(args.ignore.as_deref());

    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stderr())?;
    check_cpu::<C>(&emu, cpu)?;

    let reference = File::open(&args.reference)
        .with_context(|| format!("failed to open reference: {}", args.reference.display()))?;
    let reader = BufReader::with_capacity(256 * 1024, reference);

    let mut out = open_output(args.output.as_ref())?;
    writeln!(
        out,
        "Comparing {} trace of {} against {}",
        cpu.label,
        args.rom.path.display(),
        args.reference.display()
    )?;
    if args.skip_timing {
        if cpu.timing_columns.is_empty() {
            writeln!(
                out,
                "skip-timing: the {} lines carry no timing columns",
                cpu.label
            )?;
        } else {
            writeln!(
                out,
                "skip-timing: ignoring the {} columns",
                cpu.timing_columns
            )?;
        }
    }
    if !ignore.is_empty() {
        writeln!(out, "ignoring fields: {}", sorted_names(&ignore))?;
    }
    if args.resync > 0 {
        writeln!(out, "resync window: {} line(s)", args.resync)?;
    }

    let rules = CompareRules {
        parse: cpu.parse_line,
        skip_timing: args.skip_timing,
        ignore,
        max_errors: args.max_errors.max(1),
    };
    let mut comparator = Comparator::new(rules, reader, out, args.resync);
    let bar = progress::spinner(args.progress, &format!("comparing {}", cpu.label));
    let stats = drive_trace::<C>(
        &mut emu,
        TraceRun {
            cpu,
            limit,
            warmup: args.warmup,
            timing: true,
        },
        bar.as_ref(),
        |emu, frame| input.before_frame(emu, frame),
        |line| comparator.feed(line),
    )?;
    progress::finish(bar);
    comparator.finish(stats.frames)
}
