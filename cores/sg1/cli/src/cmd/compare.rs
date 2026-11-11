//! `compare`: diff two text trace files offline (optimized for large
//! files). Both files are streamed through 256 KB `BufReader`s; only a
//! small rolling context window is held in memory. Divergence is decided
//! on the architectural registers; `--skip-timing` tallies (does not
//! trigger on) lines where only the MAME timing columns drift.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};

use crate::trace_ref::{TraceCpu, TraceLine};

#[derive(Args, Debug)]
pub struct CompareArgs {
    /// First trace file (e.g. the sg1 dump).
    pub trace1: PathBuf,
    /// Second trace file (e.g. the MAME reference).
    pub trace2: PathBuf,
    /// Which CPU the traces describe: `m68k` (68000) or `z80`.
    #[arg(long, default_value = "m68k")]
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
    /// Comma-separated register names to ignore (e.g. `PC,SR,A7`).
    #[arg(short, long)]
    pub ignore: Option<String>,
    /// Show a progress bar.
    #[arg(long)]
    pub progress: bool,
    /// Write the report here instead of stdout.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Tally (don't trigger) timing-only divergences.
    #[arg(long)]
    pub skip_timing: bool,
}

pub fn run(args: CompareArgs) -> Result<()> {
    let cpu = TraceCpu::parse(&args.cpu)?;
    let ignore = super::parse_ignore(args.ignore.as_deref());

    let file1 = File::open(&args.trace1)
        .with_context(|| format!("failed to open trace 1: {}", args.trace1.display()))?;
    let file2 = File::open(&args.trace2)
        .with_context(|| format!("failed to open trace 2: {}", args.trace2.display()))?;
    let file1_size = file1.metadata()?.len();
    let file2_size = file2.metadata()?.len();

    let mut out = super::open_output(args.output.as_ref())?;
    writeln!(out, "=== Trace Comparison ({}) ===", cpu.label())?;
    writeln!(
        out,
        "File 1: {} ({} bytes)",
        args.trace1.display(),
        file1_size
    )?;
    writeln!(
        out,
        "File 2: {} ({} bytes)",
        args.trace2.display(),
        file2_size
    )?;
    writeln!(
        out,
        "Options: skip1={}, skip2={}, limit={}, max_errors={}",
        args.skip1,
        args.skip2,
        args.limit
            .map(|l| l.to_string())
            .unwrap_or_else(|| "unlimited".to_string()),
        args.max_errors
    )?;
    if !ignore.is_empty() {
        let mut names: Vec<&str> = ignore.iter().map(|s| s.as_str()).collect();
        names.sort_unstable();
        writeln!(out, "Ignoring: {}", names.join(", "))?;
    }
    if args.skip_timing {
        writeln!(out, "Skip-timing mode: ignoring timing-only divergences")?;
    }
    writeln!(out)?;

    let mut reader1 = BufReader::with_capacity(256 * 1024, file1);
    let mut reader2 = BufReader::with_capacity(256 * 1024, file2);

    if args.skip1 > 0 {
        skip_lines(&mut reader1, args.skip1)?;
    }
    if args.skip2 > 0 {
        skip_lines(&mut reader2, args.skip2)?;
    }

    let progress = make_bar(args.progress, args.limit);

    let mut line_num = 0u64;
    let mut divergences = 0usize;
    let mut timing_skipped = 0u64;
    let mut first_div: Option<u64> = None;
    let mut context_buf: Vec<String> = Vec::with_capacity(args.context);
    let effective_limit = args.limit.unwrap_or(u64::MAX);

    let mut line1 = String::new();
    let mut line2 = String::new();

    loop {
        if line_num >= effective_limit {
            break;
        }
        line1.clear();
        line2.clear();
        let r1 = reader1.read_line(&mut line1)?;
        let r2 = reader2.read_line(&mut line2)?;

        match (r1 > 0, r2 > 0) {
            (true, true) => {
                line_num += 1;
                let l1 = line1.trim_end();
                let l2 = line2.trim_end();

                if args.context > 0 {
                    if context_buf.len() >= args.context {
                        context_buf.remove(0);
                    }
                    context_buf.push(l1.to_string());
                }

                match (TraceLine::parse(cpu, l1), TraceLine::parse(cpu, l2)) {
                    (Some(e1), Some(e2)) => {
                        if args.skip_timing
                            && !e1.regs_differ(&e2, &ignore)
                            && e1.only_timing_differs(&e2, &ignore)
                        {
                            timing_skipped += 1;
                        } else if e1.regs_differ(&e2, &ignore) {
                            divergences += 1;
                            if first_div.is_none() {
                                first_div = Some(line_num + args.skip1);
                            }
                            writeln!(
                                out,
                                "--- Divergence #{} at line {} ---",
                                divergences,
                                line_num + args.skip1
                            )?;
                            if context_buf.len() > 1 {
                                writeln!(out, "Context (file 1):")?;
                                for (i, c) in
                                    context_buf.iter().take(context_buf.len() - 1).enumerate()
                                {
                                    let ctx_line =
                                        line_num + args.skip1 - (context_buf.len() - i - 1) as u64;
                                    writeln!(out, "  {ctx_line:>10}: {c}")?;
                                }
                            }
                            writeln!(out)?;
                            writeln!(out, "  File 1: {l1}")?;
                            writeln!(out, "  File 2: {l2}")?;
                            writeln!(out)?;
                            writeln!(out, "  Differences:")?;
                            for d in e1.register_diffs(&e2, &ignore) {
                                writeln!(out, "    {}: {} vs {}", d.field, d.ours, d.theirs)?;
                            }
                            writeln!(out)?;
                            if divergences >= args.max_errors {
                                break;
                            }
                        }
                    }
                    (None, _) => {
                        writeln!(
                            out,
                            "Warning: could not parse line {} in file 1: {l1}",
                            line_num + args.skip1
                        )?;
                    }
                    (_, None) => {
                        writeln!(
                            out,
                            "Warning: could not parse line {} in file 2: {l2}",
                            line_num + args.skip2
                        )?;
                    }
                }

                if let Some(pb) = &progress {
                    if line_num.is_multiple_of(100_000) {
                        pb.set_position(line_num);
                    }
                }
            }
            (false, false) => break,
            (true, false) => {
                writeln!(
                    out,
                    "File 2 ended early. File 1 has more data after line {}.",
                    line_num + args.skip1
                )?;
                break;
            }
            (false, true) => {
                writeln!(
                    out,
                    "File 1 ended early. File 2 has more data after line {}.",
                    line_num + args.skip2
                )?;
                break;
            }
        }
    }

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }

    writeln!(out, "=== Summary ===")?;
    writeln!(out, "Compared {line_num} lines")?;
    if timing_skipped > 0 {
        writeln!(out, "Timing-only divergences skipped: {timing_skipped}")?;
    }
    if divergences == 0 {
        writeln!(out, "Result: MATCH - No register divergences found!")?;
    } else {
        writeln!(out, "Result: DIVERGED - Found {divergences} divergence(s)")?;
        if let Some(first) = first_div {
            writeln!(out, "First divergence at line {first}")?;
        }
    }
    out.flush()?;
    Ok(())
}

/// Skip `count` lines, stopping at line boundaries (and at EOF).
fn skip_lines<R: BufRead>(reader: &mut R, count: u64) -> std::io::Result<u64> {
    let mut skipped = 0u64;
    let mut buf = String::new();
    while skipped < count {
        buf.clear();
        if reader.read_line(&mut buf)? == 0 {
            break;
        }
        skipped += 1;
    }
    Ok(skipped)
}

fn make_bar(show: bool, limit: Option<u64>) -> Option<ProgressBar> {
    if !show {
        return None;
    }
    let pb = ProgressBar::new(limit.unwrap_or(u64::MAX));
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos} lines ({elapsed})")
            .unwrap()
            .progress_chars("#>-"),
    );
    Some(pb)
}
