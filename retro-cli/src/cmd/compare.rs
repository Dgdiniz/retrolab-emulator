//! `compare`: diff two text trace files offline, line against line. Both
//! files are read as they are needed; only a few lines of context are held.

use std::path::PathBuf;

use anyhow::Result;

use crate::console::{find_cpu, Console};
use crate::output::open_output;
use crate::progress;
use crate::trace::compare::{compare_files, CompareRules, FileCompare};
use crate::trace::line::parse_ignore;

#[derive(clap::Args, Debug)]
pub struct CompareArgs {
    /// First trace file (e.g. this tool's trace).
    pub trace1: PathBuf,
    /// Second trace file (e.g. the reference).
    pub trace2: PathBuf,
    /// Which CPU the traces describe.
    #[arg(long, default_value = "")]
    pub cpu: String,
    /// Number of context lines to show before a divergence.
    #[arg(short, long, default_value_t = 3)]
    pub context: usize,
    /// Stop after this many divergences.
    #[arg(short = 'e', long, default_value_t = 1)]
    pub max_errors: usize,
    /// Skip N lines from trace1 before comparing.
    #[arg(long, default_value_t = 0)]
    pub skip1: u64,
    /// Skip N lines from trace2 before comparing.
    #[arg(long, default_value_t = 0)]
    pub skip2: u64,
    /// Limit the comparison to N lines after the skips.
    #[arg(short, long)]
    pub limit: Option<u64>,
    /// Comma-separated register or field names to ignore.
    #[arg(short, long)]
    pub ignore: Option<String>,
    /// Show a progress bar (on stderr).
    #[arg(long)]
    pub progress: bool,
    /// Write the report here instead of stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Leave the timing columns out of the comparison, counting the lines
    /// whose registers match while their timing differs.
    #[arg(long)]
    pub skip_timing: bool,
}

pub fn run<C: Console>(args: CompareArgs) -> Result<()> {
    let cpu = find_cpu::<C>(&args.cpu)?;
    let options = FileCompare {
        label: cpu.label,
        rules: CompareRules {
            parse: cpu.parse_line,
            skip_timing: args.skip_timing,
            ignore: parse_ignore(args.ignore.as_deref()),
            max_errors: args.max_errors,
        },
        context: args.context,
        skip1: args.skip1,
        skip2: args.skip2,
        limit: args.limit,
    };
    let mut out = open_output(args.output.as_ref())?;
    let bar = progress::line_bar(args.progress, args.limit);
    compare_files(&args.trace1, &args.trace2, &options, &mut out, bar.as_ref())?;
    progress::finish(bar);
    Ok(())
}
