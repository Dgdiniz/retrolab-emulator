//! `trace`: run a ROM for a bounded window while tracing one CPU, and
//! write one line per instruction in the console's reference text format.
//! `compare` diffs the output against a reference offline; `compare-trace`
//! makes the same comparison live.

use std::io::Write;
use std::ops::ControlFlow;
use std::path::PathBuf;

use anyhow::Result;

use crate::console::{find_cpu, Console};
use crate::input::InputArgs;
use crate::output::open_output;
use crate::progress;
use crate::rom::{load, write_banner, RomArgs};
use crate::trace::{check_cpu, drive_trace, RunLimit, TraceRun};

#[derive(clap::Args, Debug)]
pub struct TraceArgs<B: clap::Args> {
    #[command(flatten)]
    pub rom: RomArgs<B>,
    /// Which CPU to trace.
    #[arg(long, default_value = "")]
    pub cpu: String,
    /// Trace exactly N frames.
    #[arg(long, short)]
    pub frames: Option<u64>,
    /// Trace exactly N instructions of the main CPU.
    #[arg(long, short)]
    pub instructions: Option<u64>,
    /// Write the trace here instead of stdout.
    #[arg(long, short)]
    pub output: Option<PathBuf>,
    #[command(flatten)]
    pub input: InputArgs,
    /// Prepend the timing columns a console writes on request.
    #[arg(long)]
    pub timing: bool,
    /// Show a progress bar (on stderr).
    #[arg(long)]
    pub progress: bool,
}

pub fn run<C: Console>(args: TraceArgs<C::BootArgs>) -> Result<()> {
    let cpu = find_cpu::<C>(&args.cpu)?;
    let Some(limit) = RunLimit::from_flags(args.frames, args.instructions)? else {
        anyhow::bail!("specify --frames or --instructions");
    };
    cpu.check_available()?;
    let input = args.input.plan::<C::Emulator>()?;

    let (_, mut emu) = load::<C>(&args.rom)?;
    write_banner(&mut emu, &mut std::io::stderr())?;
    check_cpu::<C>(&emu, cpu)?;

    let mut out = open_output(args.output.as_ref())?;
    let bar = match limit {
        RunLimit::Frames(n) => progress::bar(args.progress, n, "frames"),
        RunLimit::Instructions(n) => progress::bar(args.progress, n, "instructions"),
        RunLimit::Unbounded => progress::spinner(args.progress, &format!("tracing {}", cpu.label)),
    };
    let stats = drive_trace::<C>(
        &mut emu,
        TraceRun {
            cpu,
            limit,
            warmup: 0,
            timing: args.timing,
        },
        bar.as_ref(),
        |emu, frame| input.before_frame(emu, frame),
        |line| {
            writeln!(out, "{line}")?;
            Ok(ControlFlow::Continue(()))
        },
    )?;
    out.flush()?;
    progress::finish(bar);

    // The summary goes to stderr, so a trace written to stdout stays clean.
    eprintln!(
        "Traced {} {} instructions over {} frame(s).",
        stats.instructions, cpu.label, stats.frames
    );
    if let Some(path) = &args.output {
        eprintln!("Written to {}", path.display());
    }
    Ok(())
}
