//! One module per shared command: its clap arguments and its `run`, which
//! [`crate::cli::run`] calls with the tool's [`crate::Console`].

pub mod compare;
pub mod compare_trace;
pub mod determinism_check;
pub mod disasm;
pub mod fingerprint;
pub mod info;
pub mod memory;
pub mod run;
pub mod trace;
pub mod trace_to_text;
pub mod watch_ram;
