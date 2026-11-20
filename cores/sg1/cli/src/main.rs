//! Command-line entry point for the SG1 Genesis/Mega Drive emulator: the
//! commands both tools share come from `retro-cli`, over what the Mega
//! Drive gives them (`console.rs`: its CPUs, the parsers of their
//! MAME-format lines in `trace_ref.rs`, its start-up flag); the Mega
//! Drive's own commands are under `cmd/`.

mod cmd;
mod console;
mod trace_ref;

fn main() -> anyhow::Result<()> {
    retro_cli::run::<console::Genesis>()
}
