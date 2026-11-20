//! The command line every tool parses: the shared commands and the tool's
//! own, and the texts of the help that name the console's facts.

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};

use crate::cmd::compare::CompareArgs;
use crate::cmd::compare_trace::CompareTraceArgs;
use crate::cmd::determinism_check::DeterminismCheckArgs;
use crate::cmd::disasm::DisasmArgs;
use crate::cmd::fingerprint::FingerprintArgs;
use crate::cmd::info::InfoArgs;
use crate::cmd::memory::MemoryArgs;
use crate::cmd::run::RunArgs;
use crate::cmd::trace::TraceArgs;
use crate::cmd::trace_to_text::TraceToTextArgs;
use crate::cmd::watch_ram::WatchRamArgs;
use crate::console::{cpu_help_list, join_words, CliCpu, Console};
use crate::input::button_names;

/// A tool's command line.
#[derive(clap::Parser, Debug)]
pub struct Cli<C: Console> {
    #[command(subcommand)]
    pub command: AllCommands<C>,
}

/// The shared commands, then the tool's own. The shared ones are boxed:
/// their arguments are much larger than most of a tool's own.
#[derive(clap::Subcommand, Debug)]
pub enum AllCommands<C: Console> {
    #[command(flatten)]
    Shared(Box<SharedCommand<C::BootArgs>>),
    #[command(flatten)]
    Own(C::Command),
}

/// The commands both tools run, `B` the tool's start-up flags.
#[derive(clap::Subcommand, Debug)]
pub enum SharedCommand<B: clap::Args> {
    /// Print the CLI version.
    Version,
    /// Inspect a ROM's header (title, region, checksum, mapper), its
    /// coprocessors and its CPU memory map.
    Info(InfoArgs<B>),
    /// Boot a ROM and run it for a bounded window.
    Run(RunArgs<B>),
    /// Forward-disassemble a CPU's instruction stream.
    Disasm(DisasmArgs<B>),
    /// Run a ROM and dump a region of CPU-visible memory (hex + ASCII).
    Memory(MemoryArgs<B>),
    /// Single-step a window and name the instruction that writes a RAM cell.
    WatchRam(WatchRamArgs<B>),
    /// Emit a per-frame CRC32 of a work-RAM region for divergence bisection.
    Fingerprint(FingerprintArgs<B>),
    /// Verify netplay determinism and savestate completeness over a long
    /// replay (lockstep / rollback).
    DeterminismCheck(DeterminismCheckArgs<B>),
    /// Dump an execution trace.
    Trace(TraceArgs<B>),
    /// Diff the core's live trace against a reference log.
    CompareTrace(CompareTraceArgs<B>),
    /// Diff two text trace files offline (optimized for large files).
    Compare(CompareArgs),
    /// Convert a binary trace blob to text.
    TraceToText(TraceToTextArgs),
}

/// The tool's clap command: the tool's name, about line and version, the
/// help texts that name its ROM files, its CPUs, its trace format, its
/// buttons, its work RAM and its examples, its start-up flags listed after
/// each command's own, and its commands in its order.
pub fn command<C: Console>() -> clap::Command {
    let cpus = cpu_help_list(C::CPUS);
    let main = &C::CPUS[0];
    let bits = main.address_bits;
    let digits = usize::from(bits).div_ceil(4);
    let rom_help = format!("Path to the ROM file ({})", C::ROM_FILES);
    let format = C::TRACE_FORMAT;
    let ignore_help = format!(
        "Comma-separated register or field names to ignore (e.g. `{}`)",
        C::IGNORE_EXAMPLE
    );
    let skip_timing_help = skip_timing_help(C::CPUS);
    let mut compare_cpu_help = format!("Which CPU to compare: {cpus}");
    if let Some(note) = own_trace_note(C::CPUS, format) {
        compare_cpu_help = format!("{compare_cpu_help}. {note}");
    }
    let (work_ram, work_ram_size) = C::WORK_RAM;
    let settles = match C::DECODED_WITH {
        "" => "PC settles".to_string(),
        with => format!("PC + {with} settle"),
    };

    let mut cmd = Cli::<C>::command()
        .name(C::TOOL)
        .about(C::ABOUT)
        .version(C::VERSION)
        .mut_subcommand("info", |s| {
            s.mut_arg("frames", |a| {
                a.help(format!(
                    "Run this many frames after loading before dumping coprocessor + memory \
                     state (0 = static snapshot at reset). Use to confirm a coprocessor actually \
                     fired, e.g. {}",
                    C::CHIP_ACTIVITY
                ))
            })
        })
        .mut_subcommand("run", |s| {
            s.mut_arg("instructions", |a| {
                a.help(format!(
                    "Run exactly N {} instructions; a step that only waits or only takes an \
                     interrupt is not one",
                    main.label
                ))
            })
            .mut_arg("until_vblank", |a| {
                a.help(format!(
                    "Run the first frame to the start of its vertical blanking and stop there, \
                     the picture drawn and the {} not yet raised",
                    C::VBLANK_INTERRUPT
                ))
            })
            .mut_arg("until_pc", |a| {
                a.help(format!(
                    "Step until the {} reaches this {bits}-bit PC (hex, e.g. `{}`)",
                    main.label,
                    C::PC_EXAMPLE
                ))
            })
        })
        .mut_subcommand("disasm", |s| {
            s.mut_arg("cpu", |a| {
                a.default_value(main.name())
                    .help(format!("Which CPU to disassemble: {cpus}"))
            })
            .mut_arg("frames", |a| {
                a.help(format!(
                    "Run this many frames before decoding (so {settles})"
                ))
            })
        })
        .mut_subcommand("memory", |s| {
            s.mut_arg("addr", |a| {
                a.help(format!(
                    "Start address ({bits}-bit hex, e.g. `{work_ram:0digits$x}`, \
                     `0x{work_ram:0digits$X}`, `${work_ram:0digits$X}`)"
                ))
            })
        })
        .mut_subcommand("watch-ram", |s| {
            let example = C::WATCH_EXAMPLE;
            s.mut_arg("addr", |a| {
                a.help(format!(
                    "Work-RAM address to watch ({bits}-bit hex, e.g. `{example:0digits$X}`, \
                     `0x{example:0digits$X}`)"
                ))
            })
            .mut_arg("max_steps", |a| {
                a.help(format!(
                    "Instructions to single-step while watching the cell ({})",
                    C::FRAME_INSTRUCTIONS
                ))
            })
            .mut_arg("report_pc", |a| {
                a.help(format!(
                    "Also print the watched value every time this PC is about to execute \
                     ({bits}-bit hex). Use to reproduce a known read/dispatch site"
                ))
            })
        })
        .mut_subcommand("fingerprint", |s| {
            s.mut_arg("addr", |a| {
                a.default_value(format!("{work_ram:0digits$X}"))
                    .help(format!(
                    "Region start address ({bits}-bit hex, default `{work_ram:0digits$X}` = Work \
                     RAM)"
                ))
            })
            .mut_arg("len", |a| {
                a.default_value(format!("{work_ram_size:X}")).help(format!(
                    "Region length in bytes (default `{work_ram_size:X}` = the whole {} KB Work \
                     RAM)",
                    work_ram_size / 1024
                ))
            })
        })
        .mut_subcommand("trace", |s| {
            s.about(format!("Dump an execution trace in {format} text format"))
                .mut_arg("cpu", |a| {
                    a.default_value(main.name())
                        .help(format!("Which CPU to trace: {cpus}"))
                })
                .mut_arg("instructions", |a| {
                    a.help(format!(
                        "Trace exactly N instructions ({} only)",
                        main.label
                    ))
                })
                .mut_arg("timing", |a| a.help(C::TIMING_HELP))
        })
        .mut_subcommand("compare-trace", |s| {
            s.about(format!(
                "Diff the core's live trace against a {format} reference log"
            ))
            .mut_arg("reference", |a| {
                a.help(format!("Path to the {format} reference trace log"))
            })
            .mut_arg("cpu", |a| {
                a.default_value(main.name()).help(compare_cpu_help)
            })
            .mut_arg("instructions", |a| {
                a.help(format!(
                    "Cap the comparison at N instructions ({} only)",
                    main.label
                ))
            })
            .mut_arg("ignore", |a| a.help(ignore_help.clone()))
            .mut_arg("skip_timing", |a| a.help(skip_timing_help.clone()))
        })
        .mut_subcommand("compare", |s| {
            s.mut_arg("trace1", |a| {
                a.help(format!("First trace file (e.g. the {} dump)", C::TOOL))
            })
            .mut_arg("trace2", |a| {
                a.help(format!("Second trace file (e.g. the {format} reference)"))
            })
            .mut_arg("cpu", |a| {
                a.default_value(main.name())
                    .help(format!("Which CPU the traces describe: {cpus}"))
            })
            .mut_arg("ignore", |a| a.help(ignore_help.clone()))
            .mut_arg("skip_timing", |a| a.help(skip_timing_help.clone()))
        })
        .mut_subcommand("trace-to-text", |s| {
            s.about(format!(
                "Convert a binary trace blob to {format} text format"
            ))
            .mut_arg("cpu", |a| {
                a.default_value(main.name())
                    .help(format!("Which CPU produced the trace: {cpus}"))
            })
            .mut_arg("rom", |a| a.help(trace_rom_help::<C>()))
        });

    // The shared commands' ROM names the console's files, their input flags
    // name its buttons, and their start-up flags come after their own
    // options; the tool's own commands keep their texts and their order.
    let buttons = button_names(<C::Emulator as retro_core::golden::GoldenConsole>::BUTTONS);
    let boot: Vec<String> = <C::BootArgs as clap::Args>::augment_args(clap::Command::new("boot"))
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .collect();
    let shared = <SharedCommand<C::BootArgs> as clap::Subcommand>::augment_subcommands(
        clap::Command::new("shared"),
    );
    for name in shared.get_subcommands().map(|s| s.get_name().to_string()) {
        cmd = cmd.mut_subcommand(&name, |mut s| {
            if s.get_arguments()
                .any(|arg| arg.get_id() == "rom" && arg.is_positional())
            {
                s = s.mut_arg("rom", |a| a.help(rom_help.clone()));
            }
            if INPUT_FLAGS.iter().all(|id| has_arg(&s, id)) {
                let note = input_note(&s);
                s = with_input_help(s, &buttons, &note);
            }
            for (i, id) in boot.iter().enumerate() {
                if has_arg(&s, id) {
                    s = s.mut_arg(id, |a| a.display_order(BOOT_DISPLAY_ORDER + i));
                }
            }
            s
        });
    }
    for (i, name) in C::COMMAND_ORDER.iter().enumerate() {
        cmd = cmd.mut_subcommand(name, |s| s.display_order(i));
    }
    cmd
}

/// The ids of the flags [`crate::InputArgs`] brings.
const INPUT_FLAGS: [&str; 4] = ["press_at", "press_hold", "press", "reset_at"];

/// Where the start-up flags start in a command's list of options: after
/// the command's own, before `--help`.
const BOOT_DISPLAY_ORDER: usize = 900;

fn has_arg(cmd: &clap::Command, id: &str) -> bool {
    cmd.get_arguments().any(|arg| arg.get_id() == id)
}

/// What the input flags' help of `cmd` says, after each flag's own text,
/// about the runs it makes a step at a time: they have no frame boundary
/// to apply the input at. Empty for a command that only runs frames.
fn input_note(cmd: &clap::Command) -> String {
    if has_arg(cmd, "warmup_frames") {
        return ". Applied to the `--warmup-frames` only: the single-stepped window has no frame \
                boundary to apply it at"
            .to_string();
    }
    let stepped: Vec<String> = ["instructions", "until_pc"]
        .iter()
        .filter(|id| has_arg(cmd, id))
        .map(|id| format!("`--{}`", id.replace('_', "-")))
        .collect();
    match stepped.is_empty() {
        true => String::new(),
        false => format!(
            ". Frame-bounded runs only: a run of {} has no frame boundary to apply it at",
            join_words(stepped, "or")
        ),
    }
}

/// The input flags' help, naming the console's buttons, each followed by
/// `note` ([`input_note`]).
fn with_input_help(cmd: clap::Command, buttons: &str, note: &str) -> clap::Command {
    cmd.mut_arg("press_at", |a| {
        a.help(format!(
            "Frame-scheduled player-1 input: `\"F:buttons;F:buttons\"` (e.g. \
             `\"180:start;186:;240:a,b\"`, the buttons of a set-point separated by `,` or `+`). \
             Each set-point holds its buttons from frame `F` until the next; an empty list \
             releases. It reproduces presses timed by hand, such as those a reference log was \
             captured with. Takes precedence over `--press-hold` and `--press`. Without it, \
             `--press-hold` or `--press`, the run gets no input{note}"
        ))
    })
    .mut_arg("press_hold", |a| {
        a.help(format!(
            "Hold these player-1 buttons every frame (comma-separated: `{buttons}`), to run a \
             routine while a button (a direction, say) is held. Takes precedence over \
             `--press`{note}"
        ))
    })
    .mut_arg("press", |a| {
        a.help(format!(
            "Pulse these player-1 buttons (comma-separated: `{buttons}`) on and off every 6 \
             frames, so that a screen waiting for a new press (a title, a menu) moves on and \
             the run reaches screens a run with no input cannot{note}"
        ))
    })
    .mut_arg("reset_at", |a| {
        a.help(format!(
            "Reset the console before these frames (comma-separated, e.g. `\"360,720\"`){note}"
        ))
    })
}

/// The help of `--skip-timing`, naming the timing columns of the CPUs that
/// have some.
fn skip_timing_help(cpus: &[CliCpu]) -> String {
    let mut groups: Vec<(&str, Vec<String>)> = Vec::new();
    for cpu in cpus.iter().filter(|cpu| !cpu.timing_columns.is_empty()) {
        let owner = format!("the {}'s", cpu.label);
        match groups
            .iter_mut()
            .find(|(columns, _)| *columns == cpu.timing_columns)
        {
            Some((_, owners)) => owners.push(owner),
            None => groups.push((cpu.timing_columns, vec![owner])),
        }
    }
    let columns: Vec<String> = groups
        .into_iter()
        .map(|(columns, owners)| format!("{} {columns}", join_words(owners, "and")))
        .collect();
    let which = match columns.is_empty() {
        true => String::new(),
        false => format!(" ({})", join_words(columns, "and")),
    };
    format!(
        "Leave the timing columns{which} out of the comparison, counting the lines whose \
         registers match while their timing differs"
    )
}

/// The note on the CPUs no reference emulator traces, which `compare-trace`
/// compares with a log of this tool's own; `None` when there are none.
fn own_trace_note(cpus: &[CliCpu], format: &str) -> Option<String> {
    let own: Vec<String> = cpus
        .iter()
        .filter(|cpu| cpu.compared_with_own_trace)
        .map(|cpu| format!("`{}`", cpu.name()))
        .collect();
    let (are, traces) = match own.len() {
        0 => return None,
        1 => ("is", "does not trace it"),
        2 => ("are", "traces neither"),
        _ => ("are", "traces none of them"),
    };
    Some(format!(
        "{} {are} compared with a log saved by this tool's own trace, since {format} {traces}",
        join_words(own, "and")
    ))
}

/// The help of `trace-to-text --rom`: which CPUs' lines need the ROM, and
/// what for.
fn trace_rom_help<C: Console>() -> String {
    let (mut memory, mut slot, mut none) = (Vec::new(), Vec::new(), Vec::new());
    for cpu in C::CPUS {
        let owner = format!("the {}'s", cpu.label);
        if C::trace_text_without_rom(cpu, &[]).is_some() {
            memory.push(owner);
        } else if C::CPUS.iter().filter(|other| other.id == cpu.id).count() > 1 {
            slot.push(owner);
        } else {
            none.push(owner);
        }
    }
    let mut parts = Vec::new();
    if !memory.is_empty() {
        parts.push(format!(
            "{} lines are disassembled from it (without it, their instruction column is omitted)",
            join_words(memory, "and")
        ));
    }
    if !slot.is_empty() {
        parts.push(format!(
            "{} lines need it, to know which CPU the cartridge put in their slot",
            join_words(slot, "and")
        ));
    }
    if !none.is_empty() {
        parts.push(format!("{} need none", join_words(none, "and")));
    }
    format!("The ROM the trace was made from: {}", parts.join("; "))
}

/// Parse the command line and run the command it names.
pub fn run<C: Console>() -> Result<()> {
    let mut cmd = command::<C>();
    let matches = cmd.get_matches_mut();
    let cli = Cli::<C>::from_arg_matches(&matches).unwrap_or_else(|e| e.format(&mut cmd).exit());
    match cli.command {
        AllCommands::Shared(shared) => run_shared::<C>(*shared),
        AllCommands::Own(own) => C::run_command(own),
    }
}

fn run_shared<C: Console>(command: SharedCommand<C::BootArgs>) -> Result<()> {
    match command {
        SharedCommand::Version => {
            println!("{} {}", C::TOOL, C::VERSION);
            Ok(())
        }
        SharedCommand::Info(args) => crate::cmd::info::run::<C>(args),
        SharedCommand::Run(args) => crate::cmd::run::run::<C>(args),
        SharedCommand::Disasm(args) => crate::cmd::disasm::run::<C>(args),
        SharedCommand::Memory(args) => crate::cmd::memory::run::<C>(args),
        SharedCommand::WatchRam(args) => crate::cmd::watch_ram::run::<C>(args),
        SharedCommand::Fingerprint(args) => crate::cmd::fingerprint::run::<C>(args),
        SharedCommand::DeterminismCheck(args) => crate::cmd::determinism_check::run::<C>(args),
        SharedCommand::Trace(args) => crate::cmd::trace::run::<C>(args),
        SharedCommand::CompareTrace(args) => crate::cmd::compare_trace::run::<C>(args),
        SharedCommand::Compare(args) => crate::cmd::compare::run::<C>(args),
        SharedCommand::TraceToText(args) => crate::cmd::trace_to_text::run::<C>(args),
    }
}
