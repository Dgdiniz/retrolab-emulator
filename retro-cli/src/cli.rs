//! The command line every tool parses: the shared commands and the tool's
//! own, and the texts of the help that name the console's facts.

use anyhow::Result;
use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
use clap::{CommandFactory, FromArgMatches, ValueEnum};
use retro_core::capture::images::layer_file_name;
use retro_core::golden::GoldenConsole;
use retro_core::EmulatorCore;

use crate::cmd::audio_dump::AudioDumpArgs;
use crate::cmd::benchmark::{BenchmarkArgs, Mode, HOST_STEPS_TIME_LIMIT};
use crate::cmd::compare::CompareArgs;
use crate::cmd::compare_trace::CompareTraceArgs;
use crate::cmd::determinism_check::DeterminismCheckArgs;
use crate::cmd::disasm::DisasmArgs;
use crate::cmd::fingerprint::FingerprintArgs;
use crate::cmd::golden::{GoldenArgs, GoldenCompareArgs, GoldenSetArgs};
use crate::cmd::images::ImagesArgs;
use crate::cmd::info::InfoArgs;
use crate::cmd::manifest::ManifestArgs;
use crate::cmd::memory::MemoryArgs;
use crate::cmd::profile::ProfileArgs;
use crate::cmd::run::RunArgs;
use crate::cmd::savestate::SavestateArgs;
use crate::cmd::screenshot::ScreenshotArgs;
use crate::cmd::sram::SramArgs;
use crate::cmd::stems::StemsArgs;
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
    Shared(Box<SharedCommand<C::BootArgs, C::ScreenshotArgs>>),
    #[command(flatten)]
    Own(C::Command),
}

/// The commands both tools run, `B` the tool's start-up flags and `S` its
/// own `screenshot` flags.
#[derive(clap::Subcommand, Debug)]
pub enum SharedCommand<B: clap::Args, S: clap::Args> {
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
    /// Run a ROM and save the visible picture as a PNG (+ layer dumps).
    Screenshot(ScreenshotArgs<B, S>),
    /// Run a ROM and save the video chip's images of its last frame as PNGs.
    Images(ImagesArgs<B>),
    /// Benchmark the execution tiers (fast / light-debug / +trace /
    /// fast+recording / full, in host steps).
    Benchmark(BenchmarkArgs<B>),
    /// Profile per-subsystem frame timing and rank bottlenecks.
    Profile(ProfileArgs<B>),
    /// Run a ROM with audio capture and save the output as a WAV.
    AudioDump(AudioDumpArgs<B>),
    /// Run a ROM and save each sound channel's stem and the master as WAVs.
    Stems(StemsArgs<B>),
    /// Save or load an emulator savestate.
    Savestate(SavestateArgs<B>),
    /// Dump or seed a game's battery-backed SRAM.
    Sram(SramArgs<B>),
    /// Record a ROM's output frame by frame (state, picture and audio
    /// hashes, PNGs at chosen frames) into a folder.
    Golden(GoldenArgs<B>),
    /// Record every ROM of a manifest into a folder.
    GoldenSet(GoldenSetArgs),
    /// Compare two golden folders: first differing frame and pixel diffs.
    GoldenCompare(GoldenCompareArgs),
    /// Write the cartridge manifest the core detects for a ROM, or check
    /// one.
    Manifest(ManifestArgs<B>),
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

    // The commands of pictures, sound, states and timing: the help names
    // the console's chips, its layers (read from a machine of the console),
    // its sample rate and its main CPU.
    let facts = MachineFacts::of::<C>();
    let layer_ids = join_words(
        facts
            .layers
            .iter()
            .enumerate()
            .map(|(id, name)| format!("`{id}`={name}"))
            .collect(),
        "and",
    );
    let layer_files = join_words(
        facts
            .layers
            .iter()
            .map(|name| format!("`<stem>_{}`", layer_file_name(name)))
            .collect(),
        "and",
    );
    let first_layer = facts.layers.first().cloned().unwrap_or_default();
    let mut render_layers_help = format!(
        "Also write each layer alone beside the screenshot ({layer_files}), RGBA PNGs in which \
         every pixel the layer does not cover is transparent (alpha 0). All of them come from \
         the screenshot's own frame: the run records the settings each line is drawn with (its \
         scroll, palette, registers and the video memory it read) from the first frame on, the \
         last frame stops at its capture boundary, where its picture is drawn and vertical \
         blanking has not begun, and each layer is drawn again from that frame's lines, so a \
         raster effect (a scroll, a palette or a mode changed part of the way down the screen) \
         comes out in the layers as it does in the picture"
    );
    if !C::LAYER_NOTES.is_empty() {
        render_layers_help = format!("{render_layers_help}. {}", C::LAYER_NOTES);
    }
    cmd = cmd
        .mut_subcommand("screenshot", |s| {
            s.about(format!(
                "Run a ROM and save the visible {} output as a PNG (+ layer dumps)",
                C::VIDEO_CHIP
            ))
            .mut_arg("show_only", |a| {
                a.help(format!(
                    "Isolate a single layer: render only this one ({layer_ids}). Applies the \
                     debugger layer-disable mask to every other layer, then renders one more \
                     frame so the capture reflects the mask. Diagnoses which layer owns which \
                     pixels; a value that is not a layer is refused"
                ))
            })
            .mut_arg("hide", |a| {
                a.help(format!(
                    "Hide these layers (comma-separated ids, same numbering as `--show-only`), \
                     keeping the rest. E.g. `--hide 0` drops {first_layer}; a value that is not a \
                     layer is refused"
                ))
            })
            .mut_arg("render_layers", |a| a.help(render_layers_help))
            .mut_arg("overscan", |a| a.help(C::OVERSCAN_HELP))
            .mut_arg("coprocessor_state", |a| {
                a.help(format!(
                    "After capturing, print the installed coprocessor(s) and their live state, \
                     the same dump as `info` (e.g. {}). Ties \"take a screenshot\" and \"confirm \
                     the coprocessor did its job\" into one command",
                    C::CHIP_STATE_EXAMPLE
                ))
            })
        })
        .mut_subcommand("images", |s| images_help(s, C::VIDEO_CHIP, &facts))
        .mut_subcommand("audio-dump", |s| {
            s.about(format!(
                "Run a ROM with audio capture and save the {} output as a WAV",
                C::SOUND_CHIPS
            ))
            .mut_arg("sample_rate", |a| {
                a.default_value(facts.sample_rate.to_string())
            })
            .mut_arg("native", |a| {
                a.help(format!(
                    "Write the console's own output instead: the master of its stems, at {} Hz, \
                     {}, before anything the host does to it",
                    facts.stem_rate, facts.stem_samples
                ))
            })
        })
        .mut_subcommand("stems", |s| {
            s.about(format!(
                "Run a ROM and save each stem of the {} output and the master as WAVs at {} Hz, \
                 with stems.json",
                C::SOUND_CHIPS,
                facts.stem_rate
            ))
            .mut_arg("only", |a| {
                a.help(format!(
                    "The stems to write, comma-separated ({}); every stem by default. The \
                     master is always written",
                    facts.stems.join(", ")
                ))
            })
        })
        .mut_subcommand("benchmark", |s| {
            s.mut_arg("mode", |a| a.value_parser(benchmark_modes(main.label)))
        })
        .mut_subcommand("manifest", |s| {
            s.mut_subcommand("validate", |s| {
                s.mut_arg("rom", |a| {
                    a.help(format!(
                        "Load this ROM ({}) with the manifest, the core's builder run on it, \
                         chips included, and hold the manifest's `rom.sha256` to the ROM image",
                        C::ROM_FILES
                    ))
                })
            })
        });

    // Every command's ROM names the console's files, its input flags name
    // the console's buttons, and its start-up flags and `--manifest` come
    // after its own options, in its own subcommands too: the shared
    // commands' and the tool's own alike.
    let input_help = InputHelp {
        buttons: button_names(<C::Emulator as GoldenConsole>::BUTTONS),
        buttons_note: C::BUTTONS_NOTE,
        press_example: C::PRESS_EXAMPLE,
        reset_example: C::RESET_EXAMPLE,
    };
    let boot: Vec<String> = std::iter::once("manifest".to_string())
        .chain(
            <C::BootArgs as clap::Args>::augment_args(clap::Command::new("boot"))
                .get_arguments()
                .map(|arg| arg.get_id().to_string()),
        )
        .collect();
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for name in names {
        cmd = cmd.mut_subcommand(&name, |s| shared_edits(s, &rom_help, &input_help, &boot));
    }
    cmd = cmd
        .mut_subcommand("audio-dump", |s| {
            s.mut_arg("press_at", |a| {
                let help = a.get_help().map(|h| h.to_string()).unwrap_or_default();
                a.help(format!(
                    "{help}. Most games sit silent on a title screen until something is \
                     pressed, so a capture without input records the boot jingle at best"
                ))
            })
        })
        .mut_subcommand("savestate", |s| {
            s.mut_subcommand("load", |s| {
                s.mut_arg("rom", |a| {
                    a.help(format!("{rom_help} the state was captured against"))
                })
            })
        });
    for (i, name) in C::COMMAND_ORDER.iter().enumerate() {
        cmd = cmd.mut_subcommand(name, |s| s.display_order(i));
    }
    cmd
}

/// What the help reads from a machine of the console: the rate its sound
/// is made at, the names of its picture's layers, by id, the names of the
/// layers that read a map, and the names of its video memories.
struct MachineFacts {
    sample_rate: u32,
    layers: Vec<String>,
    /// The layers that read a map ([`retro_core::video::VideoDebugger::layer_map`]).
    mapped_layers: Vec<String>,
    /// The video memories' names, as the console calls them (`VRAM`).
    memories: Vec<&'static str>,
    /// The stems' names, rate and samples.
    stems: Vec<&'static str>,
    stem_rate: u32,
    stem_samples: &'static str,
}

impl MachineFacts {
    fn of<C: Console>() -> Self {
        let mut emu = C::Emulator::new_for_golden();
        let sample_rate = emu.get_audio_info().sample_rate;
        let layout = retro_core::audio::SoundCore::stem_layout(&emu);
        let stems = layout
            .iter()
            .flat_map(|l| l.channels.iter().map(|stem| stem.name))
            .collect();
        let stem_rate = layout.as_ref().map_or(sample_rate, |l| l.rate);
        let stem_samples = match layout.map(|l| l.kind) {
            Some(retro_core::audio::SampleKind::I16) => "16-bit samples",
            _ => "32-bit float samples",
        };
        let debugger = emu.video_debugger();
        let layers: Vec<String> = (0..debugger.get_layer_count())
            .map(|id| {
                debugger
                    .get_layer_data(id)
                    .map_or_else(|| format!("layer {id}"), |data| data.name)
            })
            .collect();
        let mapped_layers = (0u32..)
            .zip(&layers)
            .filter(|&(id, _)| debugger.layer_map(id).is_some())
            .map(|(_, name)| name.clone())
            .collect();
        let memories = debugger
            .video_memories()
            .iter()
            .map(|memory| memory.name)
            .collect();
        Self {
            sample_rate,
            layers,
            mapped_layers,
            memories,
            stems,
            stem_rate,
            stem_samples,
        }
    }
}

/// The help of `images`, naming the video chip, the files of the console's
/// layers and those of its video memories.
fn images_help(s: clap::Command, video_chip: &str, facts: &MachineFacts) -> clap::Command {
    let files = |prefix: &str, layers: &[String]| {
        join_words(
            layers
                .iter()
                .map(|name| format!("`{prefix}_{}.png`", layer_file_name(name)))
                .collect(),
            "and",
        )
    };
    let memories = join_words(
        facts
            .memories
            .iter()
            .map(|name| format!("`{}.bin`", name.to_ascii_lowercase()))
            .collect(),
        "and",
    );
    s.about(format!(
        "Run a ROM and save the {video_chip}'s images of the run's last frame as PNGs: the \
         picture, each layer alone, each layer's whole map, each sprite the frame drew, the \
         tiles and the palette, with images.json listing every file"
    ))
    .after_help(
        "The picture, `picture.png`, is always written; the other images are the ones the \
         flags name, or all of them when none is named. images.json gives each file's kind, \
         size and what it is.",
    )
    .mut_arg("frames", |a| {
        a.help(
            "Frames to run, e.g. `300`. The last stops at its capture boundary, its picture \
             drawn and its vertical blank not begun, and every image is of it: the run records \
             the settings each line is drawn with from the first frame on, each layer and the \
             sprites are drawn again from that frame's lines, and the maps, the tiles and the \
             palette are read from video memory and the palette as they stand there",
        )
    })
    .mut_arg("output", |a| {
        a.help("The folder the files go to, made if missing")
    })
    .mut_arg("layers", |a| {
        a.help(format!(
            "Each layer drawn alone ({}), RGBA, transparent (alpha 0) wherever the layer draws \
             nothing",
            files("layer", &facts.layers)
        ))
    })
    .mut_arg("maps", |a| {
        a.help(format!(
            "Each layer's whole map ({}): every cell of the map the layer reads as video memory \
             holds it, with no scroll, transparent where a character's colour is 0",
            files("map", &facts.mapped_layers)
        ))
    })
    .mut_arg("sprites", |a| {
        a.help(
            "Each sprite the frame drew, `sprite_NNN.png` by its slot in the sprite table, and \
             `sprites.png`, those sprites together as the frame drew them, the size of the \
             picture",
        )
    })
    .mut_arg("tiles", |a| {
        a.help(
            "The video memory read as characters of 8 by 8 pixels of 4 bits, 16 a row, in each \
             palette line in use, one whose colours 1 to 15 are not all 0 \
             (`tiles_palette_NN.png`, NN the line); colour 0 is transparent",
        )
    })
    .mut_arg("palette", |a| {
        a.help("The palette, `palette.png`: 16 colours a row, each a square of 16 pixels")
    })
    .mut_arg("raw", |a| {
        a.help(format!(
            "Also write each video memory's bytes as the core keeps them ({memories})"
        ))
    })
}

/// `benchmark --mode`: its rows, named in the help with the console's main
/// CPU.
fn benchmark_modes(main: &str) -> impl TypedValueParser<Value = Mode> {
    let limit = HOST_STEPS_TIME_LIMIT.as_secs();
    let help = |mode: Mode| match mode {
        Mode::LightTrace => format!("LightDebug with {main} tracing active"),
        Mode::HostSteps => format!(
            "Host steps of the {main} until the frames are done, as a debugger steps (at most \
             {limit} seconds of them)"
        ),
        other => other
            .to_possible_value()
            .and_then(|value| value.get_help().map(|h| h.to_string()))
            .unwrap_or_default(),
    };
    let values: Vec<PossibleValue> = Mode::value_variants()
        .iter()
        .filter_map(|&mode| mode.to_possible_value().map(|value| value.help(help(mode))))
        .collect();
    PossibleValuesParser::new(values).map(|name| Mode::from_str(&name, false).unwrap_or(Mode::All))
}

/// The edits every command gets, and each of its own subcommands: the help
/// of its ROM and of its input flags, and its start-up flags (and
/// `--manifest`) after its own options.
fn shared_edits(
    mut s: clap::Command,
    rom_help: &str,
    input: &InputHelp,
    boot: &[String],
) -> clap::Command {
    if s.get_arguments()
        .any(|arg| arg.get_id() == "rom" && arg.is_positional())
    {
        s = s.mut_arg("rom", |a| a.help(rom_help.to_string()));
    }
    if INPUT_FLAGS.iter().all(|id| has_arg(&s, id)) {
        let note = input_note(&s);
        s = with_input_help(s, input, &note);
    }
    for (i, id) in boot.iter().enumerate() {
        let is_flag = s
            .get_arguments()
            .any(|arg| arg.get_id() == id.as_str() && !arg.is_positional());
        if is_flag {
            s = s.mut_arg(id, |a| a.display_order(BOOT_DISPLAY_ORDER + i));
        }
    }
    let nested: Vec<String> = s
        .get_subcommands()
        .map(|n| n.get_name().to_string())
        .collect();
    for name in nested {
        s = s.mut_subcommand(&name, |n| shared_edits(n, rom_help, input, boot));
    }
    s
}

/// What the input flags' help names of the console: its buttons, and its
/// notes ([`Console::BUTTONS_NOTE`], [`Console::PRESS_EXAMPLE`],
/// [`Console::RESET_EXAMPLE`]).
struct InputHelp {
    buttons: String,
    buttons_note: &'static str,
    press_example: &'static str,
    reset_example: &'static str,
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
    if has_arg(cmd, "warmup_frames") && has_arg(cmd, "max_steps") {
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

/// The input flags' help, naming the console's buttons and its notes, each
/// followed by `note` ([`input_note`]).
fn with_input_help(cmd: clap::Command, input: &InputHelp, note: &str) -> clap::Command {
    let buttons = match input.buttons_note {
        "" => format!("`{}`", input.buttons),
        extra => format!("`{}`; {extra}", input.buttons),
    };
    let press_example = match input.press_example {
        "" => String::new(),
        example => format!(", such as {example}"),
    };
    let reset_example = match input.reset_example {
        "" => String::new(),
        example => format!(", most notably {example}"),
    };
    cmd.mut_arg("press_at", |a| {
        a.help(format!(
            "Frame-scheduled player-1 input: `\"F:buttons;F:buttons\"` (e.g. \
             `\"180:start;186:;240:a,b\"`, the buttons of a set-point separated by `,` or `+`, \
             from {buttons}). Each set-point holds its buttons from frame `F` until the next; an \
             empty list releases. It reaches a given screen the same way on every run, and \
             reproduces presses timed by hand, such as those a reference log was captured with. \
             Takes precedence over `--press-hold` and `--press`. Without it, `--press-hold` or \
             `--press`, the run gets no input{note}"
        ))
    })
    .mut_arg("press_hold", |a| {
        a.help(format!(
            "Hold these player-1 buttons every frame (comma-separated: {buttons}), to run a \
             routine while a button (a direction, say) is held. Takes precedence over \
             `--press`{note}"
        ))
    })
    .mut_arg("press", |a| {
        a.help(format!(
            "Pulse these player-1 buttons (comma-separated: {buttons}) on and off every 6 \
             frames, so that a screen waiting for a new press (a title, a menu{press_example}) \
             moves on and the run reaches screens a run with no input cannot{note}"
        ))
    })
    .mut_arg("reset_at", |a| {
        a.help(format!(
            "Reset the console before these frames (comma-separated, e.g. `\"360,720\"`). \
             Battery SRAM survives a reset, so this drives reset-gated first-boot routines a \
             plain run cannot{reset_example}, letting a single run reach real gameplay{note}"
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

fn run_shared<C: Console>(command: SharedCommand<C::BootArgs, C::ScreenshotArgs>) -> Result<()> {
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
        SharedCommand::Screenshot(args) => crate::cmd::screenshot::run::<C>(args),
        SharedCommand::Images(args) => crate::cmd::images::run::<C>(args),
        SharedCommand::Benchmark(args) => crate::cmd::benchmark::run::<C>(args),
        SharedCommand::Profile(args) => crate::cmd::profile::run::<C>(args),
        SharedCommand::AudioDump(args) => crate::cmd::audio_dump::run::<C>(args),
        SharedCommand::Stems(args) => crate::cmd::stems::run::<C>(args),
        SharedCommand::Savestate(args) => crate::cmd::savestate::run::<C>(args),
        SharedCommand::Sram(args) => crate::cmd::sram::run::<C>(args),
        SharedCommand::Golden(args) => crate::cmd::golden::run::<C>(args),
        SharedCommand::GoldenSet(args) => crate::cmd::golden::run_set::<C>(args),
        SharedCommand::GoldenCompare(args) => crate::cmd::golden::run_compare::<C>(args),
        SharedCommand::Manifest(args) => crate::cmd::manifest::run::<C>(args),
    }
}
