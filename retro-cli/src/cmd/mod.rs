//! One module per shared command: its clap arguments and its `run`, which
//! [`crate::cli::run`] calls with the tool's [`crate::Console`].

pub mod compare;
pub mod compare_trace;
pub mod trace;
pub mod trace_to_text;
