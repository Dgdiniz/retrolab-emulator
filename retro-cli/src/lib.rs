//! # Retro CLI
//!
//! What the console cores' command-line tools share: the commands both
//! tools run, written once over the machine traits of `retro-core` and a
//! small [`Console`] trait each tool implements on a marker type of its
//! own. A tool's `main` is one call, [`run`]; the tool keeps only what its
//! console makes different: its start-up flags, its CPUs and the parsers
//! of their trace lines, and its own commands.
//!
//! - [`console`]: the [`Console`] trait and [`CliCpu`], a CPU the trace
//!   commands can name.
//! - [`cli`]: the command line every tool parses ([`Cli`]), its shared
//!   commands, [`command`] (the clap command with the tool's own texts) and
//!   [`run`].
//! - [`rom`]: [`RomArgs`], the ROM a command loads, the cartridge manifest
//!   `--manifest` loads it with and the tool's start-up flags,
//!   [`rom::load`] and the line a command prints about what it loaded.
//! - [`input`]: [`InputArgs`], the scripted player-1 input of a command
//!   that runs frames, and the plan made of it.
//! - [`output`]: the report or trace a command writes, to a file or to
//!   standard output.
//! - [`image`]: the PNGs the commands write (the picture, a layer).
//! - [`audio`]: the WAV a command writes, and the loudness it reports.
//! - [`parse`]: the parsers of the values the command lines take.
//! - [`progress`]: the progress bars, on standard error.
//! - [`report`]: the reports the inspection commands print (a hex dump, a
//!   disassembly row, the cartridge's memory map and chips, a CRC-32).
//! - [`singlestep`]: the front-end of a command that runs a CPU's test
//!   vectors over retro-core's runner.
//! - [`trace`]: the trace driver, the parsed trace line and its differ,
//!   and the comparison of a trace with a reference.
//! - [`cmd`]: one module per shared command.

pub mod audio;
pub mod cli;
pub mod cmd;
pub mod console;
pub mod image;
pub mod input;
pub mod output;
pub mod parse;
pub mod progress;
pub mod report;
pub mod rom;
pub mod singlestep;
pub mod trace;

pub use cli::{command, run, Cli};
pub use console::{CliCpu, Console, ProfileLine};
pub use input::{InputArgs, InputPlan};
pub use rom::RomArgs;
