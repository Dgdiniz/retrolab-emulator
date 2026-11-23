//! One module per shared command: its clap arguments and its `run`, which
//! [`crate::cli::run`] calls with the tool's [`crate::Console`].

pub mod audio_dump;
pub mod benchmark;
pub mod compare;
pub mod compare_trace;
pub mod determinism_check;
pub mod disasm;
pub mod fingerprint;
pub mod golden;
pub mod info;
pub mod manifest;
pub mod memory;
pub mod profile;
pub mod run;
pub mod savestate;
pub mod screenshot;
pub mod sram;
pub mod stems;
pub mod trace;
pub mod trace_to_text;
pub mod watch_ram;
